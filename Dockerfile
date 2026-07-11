# Multi-stage build: one image, entrypoint picks the binary (api | indexer | disclosure).
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin api --bin indexer --bin disclosure

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/api /usr/local/bin/api
COPY --from=builder /build/target/release/indexer /usr/local/bin/indexer
COPY --from=builder /build/target/release/disclosure /usr/local/bin/disclosure
COPY migrations /migrations
ENV MIGRATIONS_DIR=/migrations
ENTRYPOINT ["/bin/sh", "-c", "exec \"$@\"", "--"]
CMD ["api"]
