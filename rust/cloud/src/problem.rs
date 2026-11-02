use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct Problem {
    pub status: u16,
    pub code: &'static str,
    pub title: &'static str,
    pub detail: String,
    pub errors: Vec<FieldError>,
}
#[derive(Clone, Debug, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[derive(Serialize)]
struct Body<'a> {
    #[serde(rename = "type")]
    kind: String,
    title: &'a str,
    status: u16,
    detail: &'a str,
    instance: &'a str,
    code: &'a str,
    trace_id: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: &'a Vec<FieldError>,
}

impl Problem {
    pub fn new(
        status: u16,
        code: &'static str,
        title: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            title,
            detail: detail.into(),
            errors: Vec::new(),
        }
    }
    pub fn unauthorized() -> Self {
        Self::new(
            401,
            "unauthorized",
            "Unauthorized",
            "Google ID token could not be verified.",
        )
    }
    pub fn forbidden() -> Self {
        Self::new(
            403,
            "forbidden",
            "Forbidden",
            "This account is not permitted to use this service.",
        )
    }
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(422, "validation", "Validation error", detail)
    }
    pub fn field(
        detail: impl Into<String>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut problem = Self::validation(detail);
        problem.errors.push(FieldError {
            field: field.into(),
            message: message.into(),
        });
        problem
    }
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::new(503, "service-unavailable", "Service unavailable", detail)
    }
    pub fn internal() -> Self {
        Self::new(
            500,
            "internal-error",
            "Internal server error",
            "An unexpected error occurred.",
        )
    }
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(404, "not-found", "Not found", detail)
    }
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(409, "conflict", "Conflict", detail)
    }
    pub fn response(&self, path: &str, trace_id: &str) -> Response {
        let status = match StatusCode::from_u16(self.status) {
            Ok(status) => status,
            Err(error) => {
                eprintln!("Invalid internal problem status: {error}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = Body {
            kind: format!("https://voice-recorder/errors/{}", self.code),
            title: self.title,
            status: status.as_u16(),
            detail: &self.detail,
            instance: path,
            code: self.code,
            trace_id,
            errors: &self.errors,
        };
        let mut response = (status, axum::Json(body)).into_response();
        response.headers_mut().insert(
            "Content-Type",
            HeaderValue::from_static("application/problem+json"),
        );
        if self.status == 401 {
            response.headers_mut().insert(
                "WWW-Authenticate",
                HeaderValue::from_static("Bearer error=\"invalid_token\""),
            );
        }
        if self.status == 429 {
            response
                .headers_mut()
                .insert("Retry-After", HeaderValue::from_static("1"));
        }
        response
    }
}
impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cloud {} ({})", self.code, self.status)
    }
}
impl std::error::Error for Problem {}
