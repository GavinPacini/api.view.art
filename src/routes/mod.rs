use {axum::http::StatusCode, serde_json::json};

pub mod auth;
pub mod channel;
pub mod stream;
pub mod wallet;

/// Utility function for mapping any error into a `500 Internal Server Error`
/// response.
pub fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "status": false, "error": err.to_string() }).to_string(),
    )
}
