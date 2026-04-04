//! PROXY protocol v1/v2 编码器 + 解析器
//!
//! 编码器（发送端）：连接上游时注入 PROXY protocol 头，传递真实客户端 IP
//! 解析器（接收端）：从入站连接解析 PROXY protocol 头，提取真实客户端 IP
//!
//! 设计原则：
//! - 零拷贝解析：v2 二进制头直接从缓冲区切片解析，不做额外堆分配
//! - 栈上编码：v1 文本头和 v2 二进制头均在栈上构造，避免 format!/Vec 分配
//! - 平台无关：编码器/解析器不依赖 unix 特定类型

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

// ─────────────────────────────────────────────
// 常量
// ─────────────────────────────────────────────

/// PROXY protocol v2 签名（12 字节）
const V2_SIGNATURE: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

/// v2 版本 + PROXY 命令 = 0x21
const V2_VERSION_COMMAND_PROXY: u8 = 0x21;
/// v2 版本 + LOCAL 命令 = 0x20
const V2_VERSION_COMMAND_LOCAL: u8 = 0x20;

/// v2 地址族 + 传输协议
const V2_AF_INET_STREAM: u8 = 0x11;  // IPv4 + TCP
const V2_AF_INET6_STREAM: u8 = 0x21; // IPv6 + TCP

/// v1 最大头长度（RFC 规定 107 字节，预留 108）
const V1_MAX_HEADER_LEN: usize = 108;

// ─────────────────────────────────────────────
// 解析结果
// ─────────────────────────────────────────────

/// PROXY protocol 解析结果
#[derive(Debug, Clone)]
pub enum ProxyHeader {
    /// 包含真实客户端地址的 PROXY 命令
    Proxy {
        src: SocketAddr,
        dst: SocketAddr,
    },
    /// LOCAL 命令（健康检查等，不携带地址信息）
    Local,
}

impl ProxyHeader {
    /// 获取源地址（客户端真实 IP）
    #[inline]
    pub fn src_addr(&self) -> Option<&SocketAddr> {
        match self {
            Self::Proxy { src, .. } => Some(src),
            Self::Local => None,
        }
    }

    /// 获取源 IP 字符串（用于日志/X-Real-IP）
    #[inline]
    pub fn src_ip_str(&self) -> Option<String> {
        self.src_addr().map(|a| a.ip().to_string())
    }
}

// ─────────────────────────────────────────────
// 编码器（发送端）
// ─────────────────────────────────────────────

/// 编码 PROXY protocol v1 头（文本格式）
///
/// 返回栈上 `[u8; 108]` + 实际长度，零堆分配
/// 格式：`PROXY TCP4 <src_ip> <dst_ip> <src_port> <dst_port>\r\n`
#[inline]
pub fn encode_v1(src: SocketAddr, dst: SocketAddr) -> ([u8; V1_MAX_HEADER_LEN], usize) {
    let mut buf = [0u8; V1_MAX_HEADER_LEN];

    // "PROXY "
    buf[..6].copy_from_slice(b"PROXY ");
    let mut pos = 6;

    // 协议族
    let proto = match (src.ip(), dst.ip()) {
        (IpAddr::V4(_), IpAddr::V4(_)) => b"TCP4 ",
        (IpAddr::V6(_), IpAddr::V6(_)) => b"TCP6 ",
        // 混合地址族：使用 UNKNOWN（RFC 要求）
        _ => {
            let unknown = b"UNKNOWN\r\n";
            buf[pos..pos + unknown.len()].copy_from_slice(unknown);
            return (buf, pos + unknown.len());
        }
    };
    buf[pos..pos + 5].copy_from_slice(proto);
    pos += 5;

    // src_ip + ' '
    pos += write_ip(&mut buf[pos..], src.ip());
    buf[pos] = b' ';
    pos += 1;

    // dst_ip + ' '
    pos += write_ip(&mut buf[pos..], dst.ip());
    buf[pos] = b' ';
    pos += 1;

    // src_port + ' '
    pos += write_u16(&mut buf[pos..], src.port());
    buf[pos] = b' ';
    pos += 1;

    // dst_port + '\r\n'
    pos += write_u16(&mut buf[pos..], dst.port());
    buf[pos] = b'\r';
    buf[pos + 1] = b'\n';
    pos += 2;

    (buf, pos)
}

/// 编码 PROXY protocol v2 头（二进制格式）
///
/// 返回栈上 `[u8; 52]` + 实际长度，零堆分配
/// IPv4 = 16 + 12 = 28 字节；IPv6 = 16 + 36 = 52 字节
#[inline]
pub fn encode_v2(src: SocketAddr, dst: SocketAddr) -> ([u8; 52], usize) {
    let mut buf = [0u8; 52];

    // 签名（12 字节）
    buf[..12].copy_from_slice(&V2_SIGNATURE);
    // 版本 + 命令
    buf[12] = V2_VERSION_COMMAND_PROXY;

    match (src.ip(), dst.ip()) {
        (IpAddr::V4(src_ip), IpAddr::V4(dst_ip)) => {
            buf[13] = V2_AF_INET_STREAM;
            // 地址长度 = 12（4+4+2+2）
            buf[14] = 0;
            buf[15] = 12;
            // src_addr (4 bytes)
            buf[16..20].copy_from_slice(&src_ip.octets());
            // dst_addr (4 bytes)
            buf[20..24].copy_from_slice(&dst_ip.octets());
            // src_port (2 bytes, big-endian)
            buf[24..26].copy_from_slice(&src.port().to_be_bytes());
            // dst_port (2 bytes, big-endian)
            buf[26..28].copy_from_slice(&dst.port().to_be_bytes());
            (buf, 28)
        }
        (IpAddr::V6(src_ip), IpAddr::V6(dst_ip)) => {
            buf[13] = V2_AF_INET6_STREAM;
            // 地址长度 = 36（16+16+2+2）
            buf[14] = 0;
            buf[15] = 36;
            // src_addr (16 bytes)
            buf[16..32].copy_from_slice(&src_ip.octets());
            // dst_addr (16 bytes)
            buf[32..48].copy_from_slice(&dst_ip.octets());
            // src_port (2 bytes)
            buf[48..50].copy_from_slice(&src.port().to_be_bytes());
            // dst_port (2 bytes)
            buf[50..52].copy_from_slice(&dst.port().to_be_bytes());
            (buf, 52)
        }
        // 混合地址族：发送 LOCAL 命令（无地址负载）
        _ => {
            buf[12] = V2_VERSION_COMMAND_LOCAL;
            buf[13] = 0; // UNSPEC
            buf[14] = 0;
            buf[15] = 0; // 无地址数据
            (buf, 16)
        }
    }
}

// ─────────────────────────────────────────────
// 解析器（接收端）
// ─────────────────────────────────────────────

/// 解析错误
#[derive(Debug)]
pub enum ParseError {
    /// 数据不足，需要更多字节
    Incomplete,
    /// 无效的 PROXY protocol 头
    Invalid(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incomplete => write!(f, "PROXY protocol: 数据不足"),
            Self::Invalid(msg) => write!(f, "PROXY protocol: {}", msg),
        }
    }
}

impl std::error::Error for ParseError {}

/// 尝试从缓冲区解析 PROXY protocol 头
///
/// 返回 `Ok((header, consumed_bytes))` 或 `Err(ParseError)`
/// 零拷贝：直接从输入切片解析，不做堆分配
pub fn parse(buf: &[u8]) -> Result<(ProxyHeader, usize), ParseError> {
    if buf.len() < 8 {
        return Err(ParseError::Incomplete);
    }

    // 检测 v2 签名（前 12 字节）
    if buf.len() >= 16 && buf[..12] == V2_SIGNATURE {
        return parse_v2(buf);
    }

    // 检测 v1 前缀 "PROXY "
    if buf.starts_with(b"PROXY ") {
        return parse_v1(buf);
    }

    Err(ParseError::Invalid("既非 v1 也非 v2 格式"))
}

/// 解析 v1 文本格式
fn parse_v1(buf: &[u8]) -> Result<(ProxyHeader, usize), ParseError> {
    // 查找 \r\n 结束符
    let header_end = buf.windows(2)
        .position(|w| w == b"\r\n")
        .ok_or(if buf.len() < V1_MAX_HEADER_LEN {
            ParseError::Incomplete
        } else {
            ParseError::Invalid("v1 头超过最大长度")
        })?;

    let line = std::str::from_utf8(&buf[6..header_end])
        .map_err(|_| ParseError::Invalid("v1 头包含非 UTF-8 字节"))?;

    // "UNKNOWN" → LOCAL
    if line.starts_with("UNKNOWN") {
        return Ok((ProxyHeader::Local, header_end + 2));
    }

    // "TCP4 <src> <dst> <sport> <dport>" 或 "TCP6 ..."
    let parts: Vec<&str> = line.splitn(5, ' ').collect();
    if parts.len() != 5 {
        return Err(ParseError::Invalid("v1 字段数量不正确"));
    }

    let _proto = parts[0]; // TCP4 / TCP6
    let src_ip: IpAddr = parts[1].parse()
        .map_err(|_| ParseError::Invalid("v1 源 IP 无效"))?;
    let dst_ip: IpAddr = parts[2].parse()
        .map_err(|_| ParseError::Invalid("v1 目标 IP 无效"))?;
    let src_port: u16 = parts[3].parse()
        .map_err(|_| ParseError::Invalid("v1 源端口无效"))?;
    let dst_port: u16 = parts[4].parse()
        .map_err(|_| ParseError::Invalid("v1 目标端口无效"))?;

    Ok((
        ProxyHeader::Proxy {
            src: SocketAddr::new(src_ip, src_port),
            dst: SocketAddr::new(dst_ip, dst_port),
        },
        header_end + 2,
    ))
}

/// 解析 v2 二进制格式（零拷贝）
fn parse_v2(buf: &[u8]) -> Result<(ProxyHeader, usize), ParseError> {
    if buf.len() < 16 {
        return Err(ParseError::Incomplete);
    }

    let ver_cmd = buf[12];
    let version = ver_cmd >> 4;
    let command = ver_cmd & 0x0F;

    if version != 2 {
        return Err(ParseError::Invalid("v2 版本号不是 2"));
    }

    let addr_family = buf[13] >> 4;
    let _transport = buf[13] & 0x0F;
    let addr_len = u16::from_be_bytes([buf[14], buf[15]]) as usize;

    let total_len = 16 + addr_len;
    if buf.len() < total_len {
        return Err(ParseError::Incomplete);
    }

    // LOCAL 命令
    if command == 0 {
        return Ok((ProxyHeader::Local, total_len));
    }

    if command != 1 {
        return Err(ParseError::Invalid("v2 未知命令"));
    }

    // PROXY 命令：按地址族解析
    match addr_family {
        // AF_INET (IPv4)
        1 => {
            if addr_len < 12 {
                return Err(ParseError::Invalid("v2 IPv4 地址数据不足"));
            }
            let src_ip = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);
            let dst_ip = Ipv4Addr::new(buf[20], buf[21], buf[22], buf[23]);
            let src_port = u16::from_be_bytes([buf[24], buf[25]]);
            let dst_port = u16::from_be_bytes([buf[26], buf[27]]);
            Ok((
                ProxyHeader::Proxy {
                    src: SocketAddr::new(IpAddr::V4(src_ip), src_port),
                    dst: SocketAddr::new(IpAddr::V4(dst_ip), dst_port),
                },
                total_len,
            ))
        }
        // AF_INET6 (IPv6)
        2 => {
            if addr_len < 36 {
                return Err(ParseError::Invalid("v2 IPv6 地址数据不足"));
            }
            let mut src_bytes = [0u8; 16];
            let mut dst_bytes = [0u8; 16];
            src_bytes.copy_from_slice(&buf[16..32]);
            dst_bytes.copy_from_slice(&buf[32..48]);
            let src_ip = Ipv6Addr::from(src_bytes);
            let dst_ip = Ipv6Addr::from(dst_bytes);
            let src_port = u16::from_be_bytes([buf[48], buf[49]]);
            let dst_port = u16::from_be_bytes([buf[50], buf[51]]);
            Ok((
                ProxyHeader::Proxy {
                    src: SocketAddr::new(IpAddr::V6(src_ip), src_port),
                    dst: SocketAddr::new(IpAddr::V6(dst_ip), dst_port),
                },
                total_len,
            ))
        }
        // AF_UNIX 或 UNSPEC → 视为 LOCAL
        _ => Ok((ProxyHeader::Local, total_len)),
    }
}

// ─────────────────────────────────────────────
// 内部辅助函数（栈上序列化，零堆分配）
// ─────────────────────────────────────────────

/// 将 IP 地址写入缓冲区，返回写入字节数
#[inline]
fn write_ip(buf: &mut [u8], ip: IpAddr) -> usize {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            let mut pos = 0;
            for (i, &o) in octets.iter().enumerate() {
                if i > 0 {
                    buf[pos] = b'.';
                    pos += 1;
                }
                pos += write_u8(&mut buf[pos..], o);
            }
            pos
        }
        IpAddr::V6(v6) => {
            // 使用标准格式（不压缩），与 Nginx 一致
            let s = v6.to_string();
            let bytes = s.as_bytes();
            buf[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        }
    }
}

/// 将 u8 写入缓冲区（无前导零），返回写入字节数
#[inline]
fn write_u8(buf: &mut [u8], val: u8) -> usize {
    if val >= 100 {
        buf[0] = b'0' + val / 100;
        buf[1] = b'0' + (val / 10) % 10;
        buf[2] = b'0' + val % 10;
        3
    } else if val >= 10 {
        buf[0] = b'0' + val / 10;
        buf[1] = b'0' + val % 10;
        2
    } else {
        buf[0] = b'0' + val;
        1
    }
}

/// 将 u16 写入缓冲区（十进制，无前导零），返回写入字节数
#[inline]
fn write_u16(buf: &mut [u8], val: u16) -> usize {
    if val >= 10000 {
        buf[0] = b'0' + (val / 10000) as u8;
        buf[1] = b'0' + ((val / 1000) % 10) as u8;
        buf[2] = b'0' + ((val / 100) % 10) as u8;
        buf[3] = b'0' + ((val / 10) % 10) as u8;
        buf[4] = b'0' + (val % 10) as u8;
        5
    } else if val >= 1000 {
        buf[0] = b'0' + (val / 1000) as u8;
        buf[1] = b'0' + ((val / 100) % 10) as u8;
        buf[2] = b'0' + ((val / 10) % 10) as u8;
        buf[3] = b'0' + (val % 10) as u8;
        4
    } else if val >= 100 {
        buf[0] = b'0' + (val / 100) as u8;
        buf[1] = b'0' + ((val / 10) % 10) as u8;
        buf[2] = b'0' + (val % 10) as u8;
        3
    } else if val >= 10 {
        buf[0] = b'0' + (val / 10) as u8;
        buf[1] = b'0' + (val % 10) as u8;
        2
    } else {
        buf[0] = b'0' + val as u8;
        1
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_v1_ipv4() {
        let src = "192.168.1.100:12345".parse().unwrap();
        let dst = "10.0.0.1:80".parse().unwrap();
        let (buf, len) = encode_v1(src, dst);
        let header = std::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(header, "PROXY TCP4 192.168.1.100 10.0.0.1 12345 80\r\n");
    }

    #[test]
    fn test_encode_v1_ipv6() {
        let src: SocketAddr = "[::1]:12345".parse().unwrap();
        let dst: SocketAddr = "[::1]:80".parse().unwrap();
        let (buf, len) = encode_v1(src, dst);
        let header = std::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(header, "PROXY TCP6 ::1 ::1 12345 80\r\n");
    }

    #[test]
    fn test_encode_v2_ipv4() {
        let src: SocketAddr = "192.168.1.100:12345".parse().unwrap();
        let dst: SocketAddr = "10.0.0.1:80".parse().unwrap();
        let (buf, len) = encode_v2(src, dst);
        assert_eq!(len, 28);
        // 验证签名
        assert_eq!(&buf[..12], &V2_SIGNATURE);
        // 验证版本+命令
        assert_eq!(buf[12], 0x21);
        // 验证地址族
        assert_eq!(buf[13], 0x11);
        // 验证地址长度
        assert_eq!(u16::from_be_bytes([buf[14], buf[15]]), 12);
    }

    #[test]
    fn test_encode_v2_ipv6() {
        let src: SocketAddr = "[2001:db8::1]:12345".parse().unwrap();
        let dst: SocketAddr = "[2001:db8::2]:443".parse().unwrap();
        let (buf, len) = encode_v2(src, dst);
        assert_eq!(len, 52);
        assert_eq!(buf[13], 0x21); // AF_INET6 + STREAM
        assert_eq!(u16::from_be_bytes([buf[14], buf[15]]), 36);
    }

    #[test]
    fn test_parse_v1_ipv4() {
        let header = b"PROXY TCP4 192.168.1.100 10.0.0.1 12345 80\r\n";
        let (result, consumed) = parse(header).unwrap();
        assert_eq!(consumed, header.len());
        match result {
            ProxyHeader::Proxy { src, dst } => {
                assert_eq!(src, "192.168.1.100:12345".parse::<SocketAddr>().unwrap());
                assert_eq!(dst, "10.0.0.1:80".parse::<SocketAddr>().unwrap());
            }
            _ => panic!("期望 Proxy 结果"),
        }
    }

    #[test]
    fn test_parse_v1_unknown() {
        let header = b"PROXY UNKNOWN\r\n";
        let (result, consumed) = parse(header).unwrap();
        assert_eq!(consumed, header.len());
        assert!(matches!(result, ProxyHeader::Local));
    }

    #[test]
    fn test_parse_v2_roundtrip_ipv4() {
        let src: SocketAddr = "192.168.1.100:12345".parse().unwrap();
        let dst: SocketAddr = "10.0.0.1:80".parse().unwrap();
        let (encoded, len) = encode_v2(src, dst);
        let (result, consumed) = parse(&encoded[..len]).unwrap();
        assert_eq!(consumed, len);
        match result {
            ProxyHeader::Proxy { src: s, dst: d } => {
                assert_eq!(s, src);
                assert_eq!(d, dst);
            }
            _ => panic!("期望 Proxy 结果"),
        }
    }

    #[test]
    fn test_parse_v2_roundtrip_ipv6() {
        let src: SocketAddr = "[2001:db8::1]:12345".parse().unwrap();
        let dst: SocketAddr = "[2001:db8::2]:443".parse().unwrap();
        let (encoded, len) = encode_v2(src, dst);
        let (result, consumed) = parse(&encoded[..len]).unwrap();
        assert_eq!(consumed, len);
        match result {
            ProxyHeader::Proxy { src: s, dst: d } => {
                assert_eq!(s, src);
                assert_eq!(d, dst);
            }
            _ => panic!("期望 Proxy 结果"),
        }
    }

    #[test]
    fn test_parse_v1_roundtrip() {
        let src: SocketAddr = "192.168.1.100:12345".parse().unwrap();
        let dst: SocketAddr = "10.0.0.1:80".parse().unwrap();
        let (encoded, len) = encode_v1(src, dst);
        let (result, consumed) = parse(&encoded[..len]).unwrap();
        assert_eq!(consumed, len);
        match result {
            ProxyHeader::Proxy { src: s, dst: d } => {
                assert_eq!(s, src);
                assert_eq!(d, dst);
            }
            _ => panic!("期望 Proxy 结果"),
        }
    }

    #[test]
    fn test_parse_incomplete() {
        assert!(matches!(parse(b"PROXY"), Err(ParseError::Incomplete)));
        assert!(matches!(parse(b"PROXY TCP4 1.2.3.4"), Err(ParseError::Incomplete)));
        assert!(matches!(parse(&V2_SIGNATURE), Err(ParseError::Incomplete)));
    }

    #[test]
    fn test_parse_invalid() {
        assert!(matches!(parse(b"GET / HTTP/1.1\r\n"), Err(ParseError::Invalid(_))));
    }
}
