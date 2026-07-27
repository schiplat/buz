use std::fmt;

#[derive(Debug)]
pub enum AppError {
    AuthFailed(String),
    TokenExpired,
    Unauthorized(String),
    NotFound(String),
    Conflict(String),
    ValidationFailed(String),
    Internal(String),
    DatabaseError(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::AuthFailed(msg) => write!(f, "AuthFailed: {msg}"),
            AppError::TokenExpired => write!(f, "TokenExpired"),
            AppError::Unauthorized(msg) => write!(f, "Unauthorized: {msg}"),
            AppError::NotFound(msg) => write!(f, "NotFound: {msg}"),
            AppError::Conflict(msg) => write!(f, "Conflict: {msg}"),
            AppError::ValidationFailed(msg) => write!(f, "ValidationFailed: {msg}"),
            AppError::Internal(msg) => write!(f, "Internal: {msg}"),
            AppError::DatabaseError(e) => write!(f, "DatabaseError: {e}"),
        }
    }
}

impl std::error::Error for AppError {}

impl AppError {
    pub fn code(&self) -> i32 {
        match self {
            AppError::AuthFailed(_) => 1001,
            AppError::TokenExpired => 1002,
            AppError::Unauthorized(_) => 1003,
            AppError::NotFound(_) => 2001,
            AppError::Conflict(_) => 2002,
            AppError::ValidationFailed(_) => 3001,
            AppError::Internal(_) | AppError::DatabaseError(_) => 5000,
        }
    }
}

#[cfg(feature = "web")]
mod web_impl {
    use super::AppError;
    use axum::response::{IntoResponse, Response};
    use axum::http::StatusCode;

    impl IntoResponse for AppError {
        fn into_response(self) -> Response {
            let body = serde_json::json!({
                "code": self.code(),
                "message": self.to_string(),
                "data": null,
            });

            let status = match &self {
                AppError::AuthFailed(_) | AppError::TokenExpired => StatusCode::UNAUTHORIZED,
                AppError::Unauthorized(_) => StatusCode::FORBIDDEN,
                AppError::NotFound(_) => StatusCode::NOT_FOUND,
                AppError::Conflict(_) => StatusCode::CONFLICT,
                AppError::ValidationFailed(_) => StatusCode::UNPROCESSABLE_ENTITY,
                AppError::Internal(_) | AppError::DatabaseError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };

            (status, axum::Json(body)).into_response()
        }
    }
}

#[cfg(feature = "sqlx")]
impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::DatabaseError(e.to_string())
    }
}
