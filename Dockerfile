# Multi-stage build: compile on Alpine musl, run on minimal Alpine.
FROM rust:1.99-alpine@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY src ./src

RUN cargo build --release --locked \
    && strip target/release/streamtop

FROM alpine:3.24@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6

RUN apk add --no-cache ca-certificates libgcc \
    && adduser -D -H -u 10001 streamtop

COPY --from=builder /app/target/release/streamtop /usr/local/bin/streamtop

USER streamtop
ENTRYPOINT ["streamtop"]
