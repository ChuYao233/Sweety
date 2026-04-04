use sweety_service::Service;

use super::service::H3Service;

/// Http/3 Builder type.
/// Take in generic types of ServiceFactory for `quinn`.
pub struct H3ServiceBuilder {
    /// 0 = 自动检测（系统总内存 80% / 2MB）
    max_handlers: usize,
}

impl Default for H3ServiceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl H3ServiceBuilder {
    /// Construct a new Service Builder with given service factory.
    pub fn new() -> Self {
        H3ServiceBuilder { max_handlers: 0 }
    }

    /// 设置 H3 全局最大并发 handler 数（0 = 自动）
    pub fn max_handlers(mut self, n: usize) -> Self {
        self.max_handlers = n;
        self
    }
}

impl<S, E> Service<Result<S, E>> for H3ServiceBuilder {
    type Response = H3Service<S>;
    type Error = E;

    async fn call(&self, res: Result<S, E>) -> Result<Self::Response, Self::Error> {
        let max = self.max_handlers;
        res.map(|s| H3Service::new(s, max))
    }
}
