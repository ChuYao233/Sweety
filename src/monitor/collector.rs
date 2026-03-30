//! 指标收集器
//! 使用环形缓冲区存储最近 N 条请求记录，供分析器使用

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::Mutex;

/// 单条请求指标记录
#[derive(Debug, Clone)]
pub struct RequestRecord {
    /// 记录时间戳（Unix 时间秒）
    pub timestamp: u64,
    /// 站点名称
    pub site: String,
    /// 请求路径
    pub path: String,
    /// 请求方法
    pub method: String,
    /// 响应状态码
    pub status: u16,
    /// 响应耗时
    pub duration: Duration,
    /// 响应字节数
    pub bytes_sent: u64,
    /// 客户端 IP
    pub client_ip: String,
}

impl RequestRecord {
    /// 获取当前 Unix 时间戳（秒）
    pub fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// 环形缓冲区（固定容量，满后覆盖最旧记录）
pub struct RingBuffer<T> {
    buf: Vec<Option<T>>,
    head: usize,
    len: usize,
    capacity: usize,
}

impl<T: Clone> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: vec![None; capacity],
            head: 0,
            len: 0,
            capacity,
        }
    }

    /// 插入一条记录
    pub fn push(&mut self, item: T) {
        self.buf[self.head] = Some(item);
        self.head = (self.head + 1) % self.capacity;
        if self.len < self.capacity {
            self.len += 1;
        }
    }

    /// 返回所有有效记录的副本（从最旧到最新）
    pub fn all(&self) -> Vec<T> {
        if self.len == 0 {
            return vec![];
        }
        let start = if self.len < self.capacity {
            0
        } else {
            self.head // 环形满时，head 指向最旧
        };

        (0..self.len)
            .filter_map(|i| self.buf[(start + i) % self.capacity].clone())
            .collect()
    }

    pub fn len(&self) -> usize { self.len }
    pub fn is_empty(&self) -> bool { self.len == 0 }
}

/// 请求记录收集器（线程安全）
pub struct RequestCollector {
    buffer: Arc<Mutex<RingBuffer<RequestRecord>>>,
}

impl RequestCollector {
    /// 创建收集器，capacity 为环形缓冲区大小
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(RingBuffer::new(capacity))),
        }
    }

    /// 记录一条请求
    pub async fn record(&self, rec: RequestRecord) {
        let mut buf = self.buffer.lock().await;
        buf.push(rec);
    }

    /// 获取所有记录的快照
    pub async fn snapshot(&self) -> Vec<RequestRecord> {
        let buf = self.buffer.lock().await;
        buf.all()
    }

    /// 获取记录总数
    pub async fn len(&self) -> usize {
        self.buffer.lock().await.len()
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(path: &str, status: u16, duration_ms: u64) -> RequestRecord {
        RequestRecord {
            timestamp: RequestRecord::now_secs(),
            site: "demo".into(),
            path: path.to_string(),
            method: "GET".into(),
            status,
            duration: Duration::from_millis(duration_ms),
            bytes_sent: 512,
            client_ip: "127.0.0.1".into(),
        }
    }

    #[test]
    fn test_ring_buffer_basic() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert_eq!(buf.all(), vec![1, 2, 3]);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn test_ring_buffer_overwrite() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        buf.push(4); // 覆盖最旧的 1
        let all = buf.all();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&2));
        assert!(all.contains(&3));
        assert!(all.contains(&4));
        assert!(!all.contains(&1));
    }

    #[tokio::test]
    async fn test_collector_record_and_snapshot() {
        let collector = RequestCollector::new(100);
        collector.record(make_record("/index.html", 200, 10)).await;
        collector.record(make_record("/api/users", 404, 5)).await;

        let snap = collector.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].path, "/index.html");
        assert_eq!(snap[1].status, 404);
    }

    #[tokio::test]
    async fn test_collector_respects_capacity() {
        let collector = RequestCollector::new(5);
        for i in 0..10 {
            collector.record(make_record(&format!("/p{}", i), 200, 1)).await;
        }
        assert_eq!(collector.len().await, 5);
    }
}
