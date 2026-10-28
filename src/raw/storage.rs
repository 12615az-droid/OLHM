//! Linux storage interfaces before unit conversion or presentation.
use crate::error::InterfaceStatus;
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

pub type Text = Result<String, InterfaceStatus>;

#[derive(Debug, Clone)]
pub struct RawBlock {
    pub name: String,
    pub path: PathBuf,
    pub identity: String,
    pub parent: Option<String>,
    pub partition: bool,
    pub attrs: BTreeMap<String, Text>,
    pub metadata: BTreeMap<String, String>,
    pub stat_at: Instant,
    pub stat: Text,
    pub temperature: Text,
    pub temperature_source: Option<String>,
    pub smart: Result<serde_json::Value, InterfaceStatus>,
    pub smart_at: Instant,
}

#[derive(Debug, Clone)]
pub struct RawFsStat {
    pub fragment_size: u64,
    pub blocks: u64,
    pub free: u64,
    pub available: u64,
}

#[derive(Debug)]
pub struct RawMount {
    pub id: String,
    pub major_minor: String,
    pub root: String,
    pub target: String,
    pub options: String,
    pub fs_type: String,
    pub source: String,
    pub stats: Result<RawFsStat, InterfaceStatus>,
}

#[derive(Debug)]
pub struct RawStorageSample {
    pub at: Instant,
    pub blocks: Result<Vec<RawBlock>, InterfaceStatus>,
    pub mounts: Result<Vec<RawMount>, InterfaceStatus>,
    pub metadata_status: Result<(), InterfaceStatus>,
}
