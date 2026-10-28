//! Optional read-only CLI adapters. No shell, self-tests, privileged helpers or writes.
use super::*;
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
};

const LIMIT: u64 = 2 * 1024 * 1024;

fn stop(mut child: std::process::Child) {
    let _ = child.kill();
    // A device ioctl can remain asleep in the kernel after SIGKILL.
    // Reap it off the sampling thread so the other metrics can continue.
    let _ = thread::Builder::new()
        .name("olhm-storage-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

fn json(program: &str, args: &[&str]) -> Result<(i32, Value), InterfaceStatus> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(status)?;
    let Some(stdout) = child.stdout.take() else {
        stop(child);
        return Err(InterfaceStatus::ReadError(format!("{program}: no stdout")));
    };
    // Drain the pipe concurrently: waiting for exit first can deadlock on a full pipe.
    let reader = match thread::Builder::new()
        .name("olhm-storage-tool".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            stop(child);
            return Err(status(error));
        }
    };
    let started = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(code)) => break code.code().unwrap_or(-1),
            Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                thread::sleep(Duration::from_millis(20))
            }
            result => {
                stop(child);
                // Do not join a reader that could be held by a stalled child.
                return Err(InterfaceStatus::ReadError(format!(
                    "{program}: timeout or process error: {result:?}"
                )));
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| InterfaceStatus::ReadError(format!("{program}: reader stopped")))?
        .map_err(status)?;
    if bytes.len() > LIMIT as usize {
        return Err(InterfaceStatus::ParseError(format!(
            "{program}: output limit"
        )));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| InterfaceStatus::ParseError(error.to_string()))?;
    Ok((code, value))
}

pub(super) fn lsblk() -> Result<BTreeMap<String, BTreeMap<String, String>>, InterfaceStatus> {
    let (code, value) = json(
        "lsblk",
        &[
            "--json",
            "--bytes",
            "--output",
            "KNAME,PATH,MAJ:MIN,TYPE,FSTYPE,LABEL,UUID,PTTYPE,PARTTYPE,PARTUUID,MODEL,SERIAL,VENDOR,REV,TRAN",
        ],
    )?;
    if code != 0 {
        return Err(InterfaceStatus::ReadError(format!("lsblk exit {code}")));
    }
    parse_lsblk(&value)
}

fn parse_lsblk(
    value: &Value,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, InterfaceStatus> {
    fn visit(items: &[Value], result: &mut BTreeMap<String, BTreeMap<String, String>>) {
        for item in items {
            if let Some(name) = item["kname"].as_str() {
                let attrs = [
                    "path", "maj:min", "type", "fstype", "label", "uuid", "pttype", "parttype",
                    "partuuid", "model", "serial", "vendor", "rev", "tran",
                ]
                .into_iter()
                .filter_map(|key| {
                    item[key]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(|text| (key.into(), text.into()))
                })
                .collect();
                result.insert(name.into(), attrs);
            }
            if let Some(children) = item["children"].as_array() {
                visit(children, result);
            }
        }
    }
    let items = value["blockdevices"]
        .as_array()
        .ok_or_else(|| InterfaceStatus::ParseError("lsblk: missing blockdevices".into()))?;
    let mut result = BTreeMap::new();
    visit(items, &mut result);
    Ok(result)
}

pub(super) fn smart(name: &str, path: &Path) -> Result<Value, InterfaceStatus> {
    let device = format!("/dev/{name}");
    // -H/-A only read health/attributes. -n avoids waking a standby SATA disk.
    // smartctl uses a bitmask exit code: failing health can be valid JSON data.
    let nvme = path.components().any(|part| part.as_os_str() == "nvme");
    let ata = path.components().any(|part| {
        part.as_os_str()
            .to_str()
            .and_then(|s| s.strip_prefix("ata"))
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
    });
    let args = if nvme {
        vec!["-H", "-A", "-j", "-d", "nvme", &device]
    } else if ata {
        vec!["-H", "-A", "-j", "-d", "ata", "-n", "standby,3,5", &device]
    } else {
        vec!["-H", "-A", "-j", "-d", "scsi", "-n", "standby,3", &device]
    };
    let (_, value) = json("smartctl", &args)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn optional_lsblk_metadata_preserves_partition_signatures_and_full_models() {
        let metadata = parse_lsblk(&json!({"blockdevices":[{"kname":"sda","model":"Full model string","pttype":"gpt","children":[{"kname":"sda1","fstype":"ntfs","uuid":"fixture-uuid","parttype":"fixture-guid"}]}]})).unwrap();
        assert_eq!(metadata["sda"]["model"], "Full model string");
        assert_eq!(metadata["sda1"]["fstype"], "ntfs");
        assert_eq!(metadata["sda1"]["uuid"], "fixture-uuid");
        assert!(!metadata["sda1"].contains_key("model"));
        assert!(parse_lsblk(&json!({"unexpected":true})).is_err());
    }
}
