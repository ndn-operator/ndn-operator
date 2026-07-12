FROM clux/muslrust:stable AS builder
ARG TARGET
ARG VERSION

WORKDIR /usr/src/app

COPY src src
COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock

RUN cargo install cargo-edit --locked
RUN cargo set-version "${VERSION}"
RUN cargo build --target=${TARGET} --release --locked --bins

FROM ghcr.io/named-data/ndnd:1.5.2 AS ndnd

FROM scratch
ARG TARGET
COPY --from=builder /usr/src/app/target/${TARGET}/release/ndnctl /
COPY --from=builder /usr/src/app/target/${TARGET}/release/init /
COPY --from=builder /usr/src/app/target/${TARGET}/release/sidecar /
COPY --from=builder /usr/src/app/target/${TARGET}/release/injector /
COPY --from=ndnd /ndnd /

ENTRYPOINT ["/ndnctl"]
