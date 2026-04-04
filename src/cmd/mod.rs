//! 子命令实现模块

pub mod daemon;
pub mod reload;
pub mod run;
pub mod validate;

/// 输出 Admin REST API 文档 JSON
pub fn cmd_api_doc() {
    let doc = sweety_lib::admin_api::build_api_doc();
    println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
}

/// 输出版本信息
pub fn cmd_version() {
    println!(
        "Sweety {}\nBuilt with Rust {}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_RUST_VERSION", "(unknown)"),
    );
}
