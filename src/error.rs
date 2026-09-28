use axum::{Json, http::StatusCode, response::IntoResponse};
use rovapi_models::{ErrorDetail, ErrorResponse, error_code};

use crate::storage::StorageError;

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
            code: error_code::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: error_code::NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn query(rejection: axum::extract::rejection::QueryRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }

    pub fn storage(error: StorageError) -> Self {
        tracing::error!(error = %error, "schedule query failed");
        match error {
            StorageError::QueryTimedOut => Self {
                status: StatusCode::GATEWAY_TIMEOUT,
                code: error_code::SCHEDULE_TIMEOUT,
                message: "the schedule query timed out".to_owned(),
            },
            StorageError::WorkerStopped => Self::schedule_unavailable(),
            _ => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: error_code::INTERNAL_ERROR,
                message: "the schedule query failed".to_owned(),
            },
        }
    }

    pub fn schedule_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: error_code::SCHEDULE_UNAVAILABLE,
            message: "no schedule database is loaded".to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(ErrorResponse {
                error: ErrorDetail {
                    code: self.code.to_owned(),
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
        Json(ErrorResponse {
            error: ErrorDetail {
                code: error_code::NOT_FOUND.to_owned(),
                message: "the requested resource does not exist".to_owned(),
            },
        }),
    )
}

pub(crate) async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(ErrorResponse {
            error: ErrorDetail {
                code: error_code::METHOD_NOT_ALLOWED.to_owned(),
                message: "the requested method is not allowed for this resource".to_owned(),
            },
        }),
    )
}
