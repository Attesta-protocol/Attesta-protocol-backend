//! Request-id correlation: every response carries `x-request-id` (echoed
//! from the request if the client supplied one, generated otherwise), the
//! id is in scope for every log line emitted while handling the request,
//! and a 5xx response's JSON body includes it too — so a user-reported
//! failure ("my path request 500'd at 14:02") can be traced to the
//! underlying (deliberately client-hidden, see `error.rs`) database error
//! in one step (ISSUES-2.md Issue 18).
//!
//! Never logs request bodies or query strings — only this opaque
//! correlation id — so mailbox hints and ciphertext never reach logs.

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use tracing::Instrument;
use uuid::Uuid;

const HEADER: HeaderName = HeaderName::from_static("x-request-id");
/// Bound on an echoed client-supplied id: long enough for a UUID or a
/// typical trace id, short enough that a hostile client can't smuggle
/// arbitrary data into logs via this header.
const MAX_LEN: usize = 128;

pub async fn attach_request_id(req: Request, next: Next) -> Response {
    let incoming = req
        .headers()
        .get(&HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= MAX_LEN && s.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_owned);
    let request_id = incoming.unwrap_or_else(|| Uuid::new_v4().to_string());

    let span = tracing::info_span!("request", request_id = %request_id);
    let mut response = next.run(req).instrument(span).await;

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(HEADER, value);
    }

    if response.status().is_server_error() {
        response = embed_id_in_json_body(response, &request_id).await;
    }

    response
}

/// Best-effort: merge `"request_id"` into a JSON error body. Falls back to
/// passing the response through unchanged if the body isn't JSON (or
/// isn't UTF-8) rather than risk corrupting or dropping it.
async fn embed_id_in_json_body(response: Response, request_id: &str) -> Response {
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, 1024 * 1024).await else {
        parts.headers.remove(axum::http::header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        parts.headers.remove(axum::http::header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::from(bytes));
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "request_id".into(),
            serde_json::Value::String(request_id.to_owned()),
        );
    }
    let new_body = serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec());
    if let Ok(len) = HeaderValue::from_str(&new_body.len().to_string()) {
        parts
            .headers
            .insert(axum::http::header::CONTENT_LENGTH, len);
    }
    Response::from_parts(parts, Body::from(new_body))
}
