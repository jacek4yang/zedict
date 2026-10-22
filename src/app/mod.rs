//! 应用服务：配置与目录、联网缓存、学习库、大模型讲解与可选本地 API。
pub mod cache;
pub mod config;
pub mod explain;
#[cfg(feature = "server")]
pub mod server;
pub mod study;
