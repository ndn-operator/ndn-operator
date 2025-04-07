FROM rust:1.85.0-slim-bullseye AS builder

WORKDIR /usr/src/app

COPY src src
COPY Cargo.toml Cargo.toml

RUN cargo build --release

FROM ghcr.io/named-data/ndnd:20250405 AS ndnd

FROM debian:bullseye-slim

COPY --from=builder /usr/src/app/target/release/ndn-operator /
COPY --from=builder /usr/src/app/target/release/init /
COPY --from=builder /usr/src/app/target/release/sidecar /
COPY --from=ndnd /ndnd /

CMD ["/ndn-operator"]