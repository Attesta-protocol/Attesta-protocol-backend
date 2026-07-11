use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // Never echo database details to clients.
        tracing::error!(error = %e, "database error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal error".into(),
        }
    }
}

impl From<attesta_core::CoreError> for ApiError {
    fn from(e: attesta_core::CoreError) -> Self {
        use attesta_core::CoreError::*;
        match e {
            UnknownPool(p) => Self::not_found(format!("unknown pool: {p}")),
            CommitmentNotFound => Self::not_found("commitment not found"),
            InvalidInput(m) => Self::bad_request(m),
            Db(e) => e.into(),
            Migrate(e) => {
                tracing::error!(error = %e, "migration error");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: "internal error".into(),
                }
            }
        }
    }
}
