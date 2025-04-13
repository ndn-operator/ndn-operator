FROM rust:1.86.0-slim-bookworm AS builder

WORKDIR /usr/src/app

COPY src src
COPY Cargo.toml Cargo.toml

RUN cargo build --release

FROM ghcr.io/named-data/ndnd:20250405 AS ndnd

FROM debian:bookworm-slim

COPY --from=builder /usr/src/app/target/release/ndn-operator /
COPY --from=builder /usr/src/app/target/release/init /
COPY --from=builder /usr/src/app/target/release/sidecar /
COPY --from=builder /usr/src/app/target/release/injector /
COPY --from=ndnd /ndnd /

CMD ["/ndn-operator"]