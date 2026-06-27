//! 统一错误类型定义。

use thiserror::Error;

/// 应用统一错误类型。
#[derive(Error, Debug)]
pub enum AppError {
    #[error("{0}")]
    Msg(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),
}


