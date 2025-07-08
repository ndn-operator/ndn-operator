FROM clux/muslrust:stable AS builder
ARG TARGET

WORKDIR /usr/src/app

COPY src src
COPY Cargo.toml Cargo.toml

RUN cargo build --target=${TARGET} --release

FROM ghcr.io/named-data/ndnd:latest AS ndnd

FROM scratch
ARG TARGET
COPY --from=builder /usr/src/app/target/${TARGET}/release/ndn-operator /
COPY --from=builder /usr/src/app/target/${TARGET}/release/init /
COPY --from=builder /usr/src/app/target/${TARGET}/release/sidecar /
COPY --from=builder /usr/src/app/target/${TARGET}/release/injector /
COPY --from=ndnd /ndnd /

CMD ["/ndn-operator"]
