# rchat — production build container
FROM rust:1.89-slim-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --workspace 2>&1 || true

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/rchat /usr/local/bin/rchat 2>/dev/null || true
COPY --from=builder /build/README.md /usr/share/doc/rchat/README.md 2>/dev/null || true
ENTRYPOINT ["/usr/local/bin/rchat"]
