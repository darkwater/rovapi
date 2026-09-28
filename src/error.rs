use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

use crate::storage::StorageError;

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
        }
    }

    pub fn query(rejection: axum::extract::rejection::QueryRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }

    pub fn storage(error: StorageError) -> Self {
        tracing::error!(error = %error, "schedule query failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: "the schedule query failed".to_owned(),
        }
    }

    pub fn schedule_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "schedule_unavailable",
            message: "no schedule database is loaded".to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

pub(crate) async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: ErrorDetail {
                code: "not_found",
                message: "the requested resource does not exist".to_owned(),
            },
        }),
    )
}

pub(crate) async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(ErrorBody {
            error: ErrorDetail {
                code: "method_not_allowed",
                message: "the requested method is not allowed for this resource".to_owned(),
            },
        }),
    )
}
