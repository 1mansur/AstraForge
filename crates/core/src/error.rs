use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub category: String,
    pub recoverable: bool,
    pub cause: Option<String>,
    pub context: Box<Value>,
}
impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let category = if code.starts_with("PATH_") || code.starts_with("POLICY_") {
            "security"
        } else if code.starts_with("DB_") {
            "persistence"
        } else if code.contains("CONFLICT") || code.contains("STALE") {
            "conflict"
        } else {
            "operation"
        };
        Self {
            code: code.to_owned(),
            message: message.into(),
            category: category.into(),
            recoverable: true,
            cause: None,
            context: Box::new(Value::Null),
        }
    }
    pub fn context(mut self, context: Value) -> Self {
        self.context = Box::new(context);
        self
    }
}
impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AppError {}
impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        let (code, message) = match error.kind() {
            std::io::ErrorKind::NotFound => {
                ("NOT_FOUND", "The requested file or resource does not exist")
            }
            std::io::ErrorKind::PermissionDenied => (
                "PERMISSION_DENIED",
                "Access to the requested resource was denied",
            ),
            std::io::ErrorKind::AlreadyExists => {
                ("ALREADY_EXISTS", "The destination already exists")
            }
            std::io::ErrorKind::TimedOut => ("TIMEOUT", "The operation timed out"),
            _ => ("IO_ERROR", "The filesystem or process operation failed"),
        };
        let mut result = Self::new(code, message);
        result.cause = Some(format!("{:?}", error.kind()));
        result
    }
}
impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        let mut result = Self::new("DB_ERROR", "The local database operation failed");
        result.cause = Some(match error {
            rusqlite::Error::SqliteFailure(code, _) => format!("SQLite {}", code.extended_code),
            rusqlite::Error::QueryReturnedNoRows => "Record not found".into(),
            _ => "Database serialization or query failure".into(),
        });
        result
    }
}
impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("INVALID_JSON", "The supplied or persisted JSON is invalid")
            .context(serde_json::json!({"line": error.line(), "column": error.column()}))
    }
}
pub type Result<T> = std::result::Result<T, AppError>;
