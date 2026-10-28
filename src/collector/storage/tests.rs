use super::*;
use std::{
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "olhm-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, value: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    fn link(&self, source: &str, target: &str) {
        let target = self.0.join(target);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        symlink(self.0.join(source), target).unwrap();
    }
    fn collector(&self) -> StorageCollector {
        StorageCollector::with_roots(self.0.join("proc"), self.0.join("sys"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn topology_temperature_identity_cache_and_removal_use_only_fixture_sources() {
    let f = Fixture::new();
    f.write("sys/devices/controller/block/vda/dev", "8:0");
    f.write("sys/devices/controller/block/vda/size", "2097152");
    f.write("sys/devices/controller/block/vda/diskseq", "1");
    f.write(
        "sys/devices/controller/block/vda/stat",
        "1 0 10 0 2 0 20 0 0 0 0",
    );
    f.write("sys/devices/controller/model", "Fixture disk");
    f.write("sys/devices/controller/block/vda/vda1/partition", "1");
    f.write("sys/devices/controller/block/vda/vda1/dev", "8:1");
    f.write("sys/devices/controller/block/vda/vda1/size", "1048576");
    f.link(
        "sys/devices/controller",
        "sys/devices/controller/block/vda/device",
    );
    f.link("sys/devices/controller/block/vda", "sys/class/block/vda");
    f.link(
        "sys/devices/controller/block/vda/vda1",
        "sys/class/block/vda1",
    );
    f.write("sys/class/hwmon/hwmon0/name", "drivetemp");
    f.write("sys/class/hwmon/hwmon0/temp1_input", "35000");
    f.link("sys/devices/controller", "sys/class/hwmon/hwmon0/device");
    f.write("sys/class/hwmon/hwmon1/name", "k10temp");
    f.write("sys/class/hwmon/hwmon1/temp1_input", "99000");
    f.link("sys/devices/controller", "sys/class/hwmon/hwmon1/device");
    let mut c = f.collector();
    let first = c.sample().blocks.unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].temperature.as_deref().unwrap(), "35000");
    assert_eq!(first[1].parent.as_deref(), Some("vda"));
    assert!(first[1].partition);
    assert_eq!(first[0].smart, Err(InterfaceStatus::NotAvailable));
    f.write("sys/devices/controller/model", "Changed model");
    f.write("sys/class/hwmon/hwmon0/temp1_input", "36000");
    let second = c.sample().blocks.unwrap();
    assert_eq!(
        second[0].attrs["device/model"].as_deref().unwrap(),
        "Fixture disk"
    );
    assert_eq!(second[0].temperature.as_deref().unwrap(), "36000");
    f.write("sys/devices/controller/block/vda/diskseq", "2");
    let changed = c.sample().blocks.unwrap();
    assert_ne!(changed[0].identity, first[0].identity);
    assert_eq!(
        changed[0].attrs["device/model"].as_deref().unwrap(),
        "Changed model"
    );
    fs::remove_file(f.0.join("sys/class/block/vda1")).unwrap();
    assert_eq!(c.sample().blocks.unwrap().len(), 1);
}

#[test]
fn mountinfo_decodes_paths_handles_optional_fields_and_rejects_corruption() {
    let text = "10 1 8:1 / /mnt/my\\040disk rw shared:1 - ext4 /dev/disk\\040one rw\n11 1 0:1 / /proc rw - proc proc rw\n";
    let mounts = mounts::parse(text).unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].target, "/mnt/my disk");
    assert_eq!(mounts[0].source, "/dev/disk one");
    for text in [
        "broken",
        "1 1 8:x / / rw - ext4 /dev/a rw",
        "1 1 8:1 / /bad\\999 rw - ext4 /dev/a rw",
    ] {
        assert!(mounts::parse(text).is_err());
    }
}

#[test]
fn capacity_is_read_from_the_mountpoint_and_remote_mounts_remain_nonblocking() {
    let f = Fixture::new();
    let target = f.0.to_string_lossy();
    f.write("proc/self/mountinfo", &format!("1 1 8:1 / {target} rw - ext4 /dev/fixture rw\n2 1 0:1 / /does-not-exist rw - nfs server:/share rw\n"));
    let mounts = f.collector().sample().mounts.unwrap();
    let stats = mounts[0].stats.as_ref().unwrap();
    assert!(stats.blocks > 0);
    assert!(stats.fragment_size > 0);
    assert!(matches!(
        mounts[1].stats,
        Err(InterfaceStatus::NotAvailable)
    ));
}
