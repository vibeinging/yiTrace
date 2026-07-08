//! 轻量元数据账本：给 trace/span 做标注，或把 trace/span 关联到回归数据集。
//!
//! 这层不参与 WAL、折叠和段生命周期。它单独落盘，避免为了产品侧审核/数据集信息去改承重的 trace 格式。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

const MAGIC: u32 = 0x5954_4D44; // YTMD
const FORMAT_VER: u32 = 2;

include!("metadata/types.rs");
include!("metadata/codec.rs");

#[cfg(test)]
mod tests;
