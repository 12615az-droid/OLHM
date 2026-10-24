use crate::error::CollectError;
pub mod cpu;
pub mod gpu;
pub mod memory;
pub mod network;
pub mod power;
pub mod sensors;
pub mod storage;

pub trait Collector {
    type Output;
    fn collect(&self) -> Result<Self::Output, CollectError>;
}
