//! The sync engine. It compares two folders. Then it makes the changes.

pub mod config;
pub mod copy;
pub mod exec;
pub mod history;
pub mod names;
pub mod plan;
pub mod scan;
pub mod sync;
pub mod volumes;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Source,
    Copy,
}

impl Side {
    pub const BOTH: [Side; 2] = [Side::Source, Side::Copy];

    pub fn other(self) -> Side {
        match self {
            Side::Source => Side::Copy,
            Side::Copy => Side::Source,
        }
    }

    pub fn ix(self) -> usize {
        match self {
            Side::Source => 0,
            Side::Copy => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Mirror,
    AddOnly,
    TwoWay,
}

pub fn now_ms() -> i64 {
    names::to_ns(std::time::SystemTime::now()) / 1_000_000
}
