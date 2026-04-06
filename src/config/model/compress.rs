//! 压缩配置（gzip / brotli / zstd）
//!
//! 全局和站点粒度共用此结构，三种算法独立开关和压缩等级。
//! 站点配置中的字段会覆盖全局默认值；未设置的字段继承全局。

use serde::{Deserialize, Serialize};

fn default_gzip_level() -> u32 { 6 }
fn default_brotli_level() -> u32 { 4 }
fn default_zstd_level() -> u32 { 3 }
fn default_min_length() -> usize { 1 }

/// 压缩配置（全局或站点级）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompressConfig {
    /// 是否启用 gzip（默认 false）
    #[serde(default)]
    pub gzip: bool,

    /// gzip 压缩等级 1-9（默认 5，等价 Nginx gzip_comp_level）
    #[serde(default = "default_gzip_level")]
    pub gzip_level: u32,

    /// 是否启用 brotli（默认 false）
    #[serde(default)]
    pub brotli: bool,

    /// brotli 压缩等级 0-11（默认 4）
    #[serde(default = "default_brotli_level")]
    pub brotli_level: u32,

    /// 是否启用 zstd（默认 false）
    #[serde(default)]
    pub zstd: bool,

    /// zstd 压缩等级 1-22（默认 3）
    #[serde(default = "default_zstd_level")]
    pub zstd_level: u32,

    /// 触发压缩的最小文件大小（KB，默认 1KB，等价 Nginx gzip_min_length）
    #[serde(default = "default_min_length")]
    pub min_length: usize,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            gzip:         true,
            gzip_level:   default_gzip_level(),
            brotli:       true,
            brotli_level: default_brotli_level(),
            zstd:         true,
            zstd_level:   default_zstd_level(),
            min_length:   default_min_length(),
        }
    }
}

/// 站点级压缩配置覆盖（所有字段均为 Option，未设置则继承全局）
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SiteCompressConfig {
    /// 覆盖全局 gzip 开关
    #[serde(default)]
    pub gzip: Option<bool>,

    /// 覆盖全局 gzip 压缩等级
    #[serde(default)]
    pub gzip_level: Option<u32>,

    /// 覆盖全局 brotli 开关
    #[serde(default)]
    pub brotli: Option<bool>,

    /// 覆盖全局 brotli 压缩等级
    #[serde(default)]
    pub brotli_level: Option<u32>,

    /// 覆盖全局 zstd 开关
    #[serde(default)]
    pub zstd: Option<bool>,

    /// 覆盖全局 zstd 压缩等级
    #[serde(default)]
    pub zstd_level: Option<u32>,

    /// 覆盖全局最小压缩文件大小（KB）
    #[serde(default)]
    pub min_length: Option<usize>,
}

impl SiteCompressConfig {
    /// 将站点配置叠加到全局配置，生成最终生效的 CompressConfig
    pub fn resolve(&self, global: &CompressConfig) -> CompressConfig {
        CompressConfig {
            gzip:         self.gzip.unwrap_or(global.gzip),
            gzip_level:   self.gzip_level.unwrap_or(global.gzip_level),
            brotli:       self.brotli.unwrap_or(global.brotli),
            brotli_level: self.brotli_level.unwrap_or(global.brotli_level),
            zstd:         self.zstd.unwrap_or(global.zstd),
            zstd_level:   self.zstd_level.unwrap_or(global.zstd_level),
            min_length:   self.min_length.unwrap_or(global.min_length),
        }
    }
}
