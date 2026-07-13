//! Abuse protection: per-IP token buckets and SSE connection caps.
//!
//! Hand-rolled (no extra deps): one bucket per client IP, refilled at a
//! sustained rate up to a burst ceiling. Every limit is disable-able via
//! config (0 = off) so private deployments behave exactly as before. The
//! client IP comes from the socket peer address — these defaults protect a
//! directly exposed API; behind a reverse proxy, apply limits there too.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::state::AppState;

/// Per-IP token buckets with lazy refill.
pub struct IpBuckets {
    per_sec: u32,
    burst: u32,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl IpBuckets {
    pub fn new(per_sec: u32, burst: u32) -> Self {
        Self {
            per_sec,
            // A burst below the sustained rate would make the sustained
            // rate unreachable; clamp up.
            burst: burst.max(per_sec),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.per_sec > 0
    }

    /// Take one token for `ip` at time `now`. On refusal, returns the
    /// seconds until a token is available (for Retry-After).
    pub fn try_acquire(&self, ip: IpAddr, now: Instant) -> Result<(), u64> {
        if !self.enabled() {
            return Ok(());
        }
        let mut buckets = self.buckets.lock().expect("bucket lock poisoned");

        // Opportunistic cleanup: full buckets carry no state worth keeping.
        if buckets.len() > 10_000 {
            let burst = self.burst as f64;
            let per_sec = self.per_sec as f64;
            buckets.retain(|_, b| {
                let refilled = (b.tokens
                    + now.duration_since(b.last_refill).as_secs_f64() * per_sec)
                    .min(burst);
                refilled < burst
            });
        }

        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.burst as f64,
            last_refill: now,
        });
        bucket.tokens = (bucket.tokens
            + now.duration_since(bucket.last_refill).as_secs_f64() * self.per_sec as f64)
            .min(self.burst as f64);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - bucket.tokens) / self.per_sec as f64).ceil() as u64)
        }
    }
}

/// Concurrent-SSE accounting: a global count plus one per IP. Guards
/// decrement on drop, so a connection is released even if the stream is
/// cancelled mid-flight.
pub struct SseSlots {
    per_ip_cap: u32,
    global_cap: u32,
    global: AtomicI64,
    per_ip: Mutex<HashMap<IpAddr, i64>>,
}

impl SseSlots {
    pub fn new(per_ip_cap: u32, global_cap: u32) -> Self {
        Self {
            per_ip_cap,
            global_cap,
            global: AtomicI64::new(0),
            per_ip: Mutex::new(HashMap::new()),
        }
    }

    pub fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Option<SseSlot> {
        if self.global_cap > 0 && self.global.load(Ordering::Relaxed) >= self.global_cap as i64 {
            return None;
        }
        {
            let mut per_ip = self.per_ip.lock().expect("sse lock poisoned");
            let count = per_ip.entry(ip).or_insert(0);
            if self.per_ip_cap > 0 && *count >= self.per_ip_cap as i64 {
                return None;
            }
            *count += 1;
        }
        self.global.fetch_add(1, Ordering::Relaxed);
        Some(SseSlot {
            slots: Arc::clone(self),
            ip,
        })
    }

    pub fn active(&self) -> i64 {
        self.global.load(Ordering::Relaxed)
    }
}

/// RAII slot; dropping it releases the SSE connection accounting.
pub struct SseSlot {
    slots: Arc<SseSlots>,
    ip: IpAddr,
}

impl Drop for SseSlot {
    fn drop(&mut self) {
        self.slots.global.fetch_sub(1, Ordering::Relaxed);
        let mut per_ip = self.slots.per_ip.lock().expect("sse lock poisoned");
        if let Some(count) = per_ip.get_mut(&self.ip) {
            *count -= 1;
            if *count <= 0 {
                per_ip.remove(&self.ip);
            }
        }
    }
}

/// 429 with Retry-After and the same JSON error shape as ApiError.
pub fn too_many_requests(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
        Json(json!({ "error": "rate limit exceeded" })),
    )
        .into_response()
}

/// Middleware over the read routes (tree, notes, credentials pickup, …).
pub async fn limit_reads(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    match state.read_buckets.try_acquire(addr.ip(), Instant::now()) {
        Ok(()) => next.run(req).await,
        Err(retry) => too_many_requests(retry),
    }
}

/// Middleware over the write routes (credential delivery, claims).
pub async fn limit_writes(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    match state.write_buckets.try_acquire(addr.ip(), Instant::now()) {
        Ok(()) => next.run(req).await,
        Err(retry) => too_many_requests(retry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([127, 0, 0, last])
    }

    #[test]
    fn burst_then_throttle_then_refill() {
        let buckets = IpBuckets::new(10, 5);
        let t0 = Instant::now();
        for _ in 0..10 {
            // burst clamps up to per_sec
            assert!(buckets.try_acquire(ip(1), t0).is_ok());
        }
        let retry = buckets.try_acquire(ip(1), t0).unwrap_err();
        assert!(retry >= 1);

        // 500 ms refills 5 tokens at 10/s.
        let t1 = t0 + Duration::from_millis(500);
        for _ in 0..5 {
            assert!(buckets.try_acquire(ip(1), t1).is_ok());
        }
        assert!(buckets.try_acquire(ip(1), t1).is_err());
    }

    #[test]
    fn ips_do_not_share_buckets() {
        let buckets = IpBuckets::new(1, 1);
        let t0 = Instant::now();
        assert!(buckets.try_acquire(ip(1), t0).is_ok());
        assert!(buckets.try_acquire(ip(1), t0).is_err());
        assert!(buckets.try_acquire(ip(2), t0).is_ok());
    }

    #[test]
    fn zero_rate_disables_limit() {
        let buckets = IpBuckets::new(0, 0);
        let t0 = Instant::now();
        for _ in 0..1000 {
            assert!(buckets.try_acquire(ip(1), t0).is_ok());
        }
    }

    #[test]
    fn tokens_never_exceed_burst() {
        let buckets = IpBuckets::new(10, 20);
        let t0 = Instant::now();
        // Long idle must not accumulate beyond the burst ceiling.
        let t1 = t0 + Duration::from_secs(3600);
        for _ in 0..20 {
            assert!(buckets.try_acquire(ip(1), t1).is_ok());
        }
        assert!(buckets.try_acquire(ip(1), t1).is_err());
    }

    #[test]
    fn sse_slots_cap_per_ip_and_globally_and_release_on_drop() {
        let slots = Arc::new(SseSlots::new(2, 3));
        let a1 = slots.try_acquire(ip(1)).expect("first per-ip slot");
        let _a2 = slots.try_acquire(ip(1)).expect("second per-ip slot");
        assert!(slots.try_acquire(ip(1)).is_none(), "per-ip cap");

        let _b1 = slots.try_acquire(ip(2)).expect("other ip fits");
        assert!(slots.try_acquire(ip(3)).is_none(), "global cap");
        assert_eq!(slots.active(), 3);

        drop(a1);
        assert_eq!(slots.active(), 2);
        assert!(slots.try_acquire(ip(3)).is_some(), "slot freed by drop");
    }

    #[test]
    fn sse_zero_caps_disable_limits() {
        let slots = Arc::new(SseSlots::new(0, 0));
        let mut held = Vec::new();
        for n in 0..50 {
            held.push(slots.try_acquire(ip(n as u8)).expect("unlimited"));
        }
    }
}
