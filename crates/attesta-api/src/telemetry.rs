//! Request telemetry: per-route counters and latency histograms.
//!
//! Labels use the matched route pattern (`/v1/tree/{pool}/path`), never
//! raw URLs, so label cardinality stays bounded and no query values (e.g.
//! recipient hints) leak into metrics.

use std::time::Instant;

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};

pub async fn track_requests(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let method = req.method().as_str().to_owned();

    let started = Instant::now();
    let response = next.run(req).await;

    let labels = [
        ("route", route),
        ("method", method),
        ("status", response.status().as_u16().to_string()),
    ];
    metrics::counter!("attesta_api_requests_total", &labels).increment(1);
    metrics::histogram!("attesta_api_request_duration_seconds", &labels)
        .record(started.elapsed().as_secs_f64());
    response
}
