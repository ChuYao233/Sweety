//! Real IP 中间件
//!
//! 等价 Nginx `set_real_ip_from` + `real_ip_header`。
//! 当 Sweety 部署在多层代理（CDN / LB）后面时，从指定请求头提取真实客户端 IP。
//!
//! 设计要点：
//! - 启动时预编译 CIDR 列表 → 运行时 O(N) 检查（受信代理通常 < 10 条）
//! - 仅当连接 IP 在受信范围内才替换，防止伪造
//! - 支持 X-Forwarded-For（取最右侧非受信 IP）和 X-Real-IP（直接取值）

use std::net::IpAddr;
use serde::{Deserialize, Serialize};

/// real_ip 配置（站点级别）
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RealIpConfig {
    /// 受信代理 IP / CIDR 列表（等价 Nginx set_real_ip_from）
    /// 仅当连接 IP 匹配这些地址时，才从 header 提取真实 IP
    #[serde(default)]
    pub set_real_ip_from: Vec<String>,

    /// 从哪个请求头读取真实 IP（默认 "X-Forwarded-For"）
    /// 等价 Nginx real_ip_header
    #[serde(default = "default_real_ip_header")]
    pub real_ip_header: String,

    /// 是否递归查找（X-Forwarded-For 从右向左跳过所有受信 IP）
    /// 等价 Nginx real_ip_recursive
    #[serde(default)]
    pub recursive: bool,
}

fn default_real_ip_header() -> String { "X-Forwarded-For".to_string() }

/// 预编译后的 real_ip 配置（启动时构建，运行时零分配）
#[derive(Debug, Clone)]
pub struct CompiledRealIp {
    /// 受信代理 CIDR 列表
    trusted: Vec<CidrEntry>,
    /// 请求头名称（小写化，便于匹配）
    header: String,
    /// 是否递归
    recursive: bool,
}

#[derive(Debug, Clone)]
struct CidrEntry {
    network: IpAddr,
    prefix_len: u8,
}

impl CidrEntry {
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(pos) = s.find('/') {
            let ip: IpAddr = s[..pos].parse().ok()?;
            let prefix_len: u8 = s[pos + 1..].parse().ok()?;
            Some(Self { network: ip, prefix_len })
        } else {
            // 单个 IP → /32 或 /128
            let ip: IpAddr = s.parse().ok()?;
            let prefix_len = if ip.is_ipv4() { 32 } else { 128 };
            Some(Self { network: ip, prefix_len })
        }
    }

    #[inline]
    fn contains(&self, ip: &IpAddr) -> bool {
        match (&self.network, ip) {
            (IpAddr::V4(net), IpAddr::V4(addr)) => {
                if self.prefix_len == 0 { return true; }
                let mask = u32::MAX.checked_shl(32 - self.prefix_len as u32).unwrap_or(0);
                (u32::from(*net) & mask) == (u32::from(*addr) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(addr)) => {
                if self.prefix_len == 0 { return true; }
                let mask = u128::MAX.checked_shl(128 - self.prefix_len as u32).unwrap_or(0);
                (u128::from(*net) & mask) == (u128::from(*addr) & mask)
            }
            _ => false,
        }
    }
}

impl CompiledRealIp {
    /// 从配置编译（启动时调用一次）
    pub fn compile(cfg: &RealIpConfig) -> Option<Self> {
        if cfg.set_real_ip_from.is_empty() {
            return None; // 未配置则不启用
        }
        let trusted: Vec<CidrEntry> = cfg.set_real_ip_from.iter()
            .filter_map(|s| {
                CidrEntry::parse(s).or_else(|| {
                    tracing::warn!("real_ip: 无效的受信代理地址 '{}'，已跳过", s);
                    None
                })
            })
            .collect();
        if trusted.is_empty() {
            return None;
        }
        Some(Self {
            trusted,
            header: cfg.real_ip_header.clone(),
            recursive: cfg.recursive,
        })
    }

    /// 返回请求头名称
    #[inline]
    pub fn header_name(&self) -> &str {
        &self.header
    }

    /// 判断给定 IP 是否为受信代理
    #[inline]
    fn is_trusted(&self, ip: &IpAddr) -> bool {
        self.trusted.iter().any(|c| c.contains(ip))
    }

    /// 从请求头中提取真实客户端 IP
    ///
    /// 返回 `Some(real_ip)` 表示成功提取，`None` 表示不替换（连接 IP 不受信或头不存在）
    pub fn extract_real_ip(&self, conn_ip: &IpAddr, header_value: Option<&str>) -> Option<IpAddr> {
        // 连接 IP 必须在受信列表内
        if !self.is_trusted(conn_ip) {
            return None;
        }

        let header_val = header_value?;

        if self.header.eq_ignore_ascii_case("X-Forwarded-For") {
            // X-Forwarded-For: client, proxy1, proxy2
            // 非递归：取最右侧一个
            // 递归：从右向左跳过所有受信 IP，取第一个非受信 IP
            let parts: Vec<&str> = header_val.split(',').map(|s| s.trim()).collect();
            if self.recursive {
                for part in parts.iter().rev() {
                    if let Ok(ip) = part.parse::<IpAddr>() {
                        if !self.is_trusted(&ip) {
                            return Some(ip);
                        }
                    }
                }
                // 全部受信，取最左侧
                parts.first().and_then(|s| s.parse().ok())
            } else {
                // 非递归：取最右侧
                parts.last().and_then(|s| s.parse().ok())
            }
        } else {
            // X-Real-IP 或其他：直接解析头值
            header_val.trim().parse().ok()
        }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cfg(trusted: &[&str], header: &str, recursive: bool) -> RealIpConfig {
        RealIpConfig {
            set_real_ip_from: trusted.iter().map(|s| s.to_string()).collect(),
            real_ip_header: header.to_string(),
            recursive,
        }
    }

    #[test]
    fn test_xff_non_recursive() {
        let cfg = make_cfg(&["10.0.0.0/8"], "X-Forwarded-For", false);
        let compiled = CompiledRealIp::compile(&cfg).unwrap();
        let conn_ip: IpAddr = "10.0.0.1".parse().unwrap();
        // 取最右侧
        let real = compiled.extract_real_ip(&conn_ip, Some("1.2.3.4, 10.0.0.2")).unwrap();
        assert_eq!(real, "10.0.0.2".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_xff_recursive() {
        let cfg = make_cfg(&["10.0.0.0/8", "172.16.0.0/12"], "X-Forwarded-For", true);
        let compiled = CompiledRealIp::compile(&cfg).unwrap();
        let conn_ip: IpAddr = "10.0.0.1".parse().unwrap();
        // 从右向左跳过受信 IP
        let real = compiled.extract_real_ip(&conn_ip, Some("1.2.3.4, 172.16.1.1, 10.0.0.2")).unwrap();
        assert_eq!(real, "1.2.3.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_x_real_ip() {
        let cfg = make_cfg(&["10.0.0.1"], "X-Real-IP", false);
        let compiled = CompiledRealIp::compile(&cfg).unwrap();
        let conn_ip: IpAddr = "10.0.0.1".parse().unwrap();
        let real = compiled.extract_real_ip(&conn_ip, Some("8.8.8.8")).unwrap();
        assert_eq!(real, "8.8.8.8".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_untrusted_conn_ip_returns_none() {
        let cfg = make_cfg(&["10.0.0.0/8"], "X-Forwarded-For", false);
        let compiled = CompiledRealIp::compile(&cfg).unwrap();
        let conn_ip: IpAddr = "8.8.8.8".parse().unwrap(); // 不在受信列表
        assert!(compiled.extract_real_ip(&conn_ip, Some("1.2.3.4")).is_none());
    }

    #[test]
    fn test_empty_config_returns_none() {
        let cfg = make_cfg(&[], "X-Forwarded-For", false);
        assert!(CompiledRealIp::compile(&cfg).is_none());
    }
}
