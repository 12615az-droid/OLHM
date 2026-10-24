use crate::raw::cpu::{RawCpuCounters, RawProcStat};
use std::collections::BTreeMap;

pub fn parse(text: &str) -> Result<RawProcStat, String> {
    parse_impl(text, true)
}

/// Monitoring needs CPU counters and boot identity only, not interrupt lists
/// or another copy of the full /proc/stat text on every polling cycle.
pub fn parse_monitor(text: &str) -> Result<RawProcStat, String> {
    parse_impl(text, false)
}

fn parse_impl(text: &str, keep_raw: bool) -> Result<RawProcStat, String> {
    let mut result = RawProcStat {
        text: if keep_raw {
            text.to_owned()
        } else {
            String::new()
        },
        aggregate: None,
        cpus: BTreeMap::new(),
        other: Vec::new(),
    };
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().ok_or("missing stat key")?;
        let value = parts.next().unwrap_or("").trim();
        let suffix = key.strip_prefix("cpu");
        if key == "cpu"
            || suffix.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            let counters = value
                .split_whitespace()
                .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            if counters.len() < 4 {
                return Err(format!("{key}: fewer than four CPU counters"));
            }
            let counters = RawCpuCounters { counters };
            if key == "cpu" {
                if result.aggregate.replace(counters).is_some() {
                    return Err("duplicate aggregate CPU".into());
                }
            } else {
                let id = key[3..].parse::<u32>().map_err(|e| e.to_string())?;
                if result.cpus.insert(id, counters).is_some() {
                    return Err(format!("duplicate {key}"));
                }
            }
        } else if keep_raw || key == "btime" {
            result.other.push((key.to_owned(), value.to_owned()));
        }
    }
    if result.aggregate.is_none() {
        return Err("missing aggregate CPU counters".into());
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counters_and_extensions() {
        let data = "cpu 1 2 3 4 5 6 7 8 9 10 11\ncpu17 9 8 7 6\nintr 4 2 2\n";
        let parsed = parse(data).unwrap();
        assert_eq!(
            parsed.aggregate.unwrap().counters,
            (1..=11).collect::<Vec<_>>()
        );
        assert_eq!(parsed.cpus[&17].counters, vec![9, 8, 7, 6]);
        assert_eq!(parsed.other[0], ("intr".into(), "4 2 2".into()));
        assert_eq!(parsed.text, data);
    }
    #[test]
    fn rejects_malformed() {
        for text in [
            "",
            "cpu 1 2 3",
            "cpu 1 x 3 4",
            "cpu -1 2 3 4",
            "cpu 1 2 3 18446744073709551616",
            "cpu 1 2 3 4\ncpu 1 2 3 4",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn monitor_keeps_only_counters_and_boot_identity() {
        let text = "cpu 10 20 30 40\ncpu7 1 2 3 4\nintr 999 1 2 3\nbtime 12345\nsoftirq 1 2 3\n";
        let raw = parse(text).unwrap();
        let monitor = parse_monitor(text).unwrap();
        assert_eq!(raw.aggregate, monitor.aggregate);
        assert_eq!(raw.cpus, monitor.cpus);
        assert!(monitor.text.is_empty());
        assert_eq!(monitor.other, [("btime".into(), "12345".into())]);
        assert!(parse_monitor("cpu 1 2 broken 4").is_err());
    }
}
