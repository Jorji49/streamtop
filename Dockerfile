# Multi-stage build: compile on Alpine musl, run on minimal Alpine.
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY LICENSE README.md CHANGELOG.md ./

RUN cargo build --release --locked \
    && strip target/release/streamtop

FROM alpine:3.21

RUN apk add --no-cache ca-certificates libgcc \
    && adduser -D -H -u 10001 streamtop

COPY --from=builder /app/target/release/streamtop /usr/local/bin/streamtop

USER streamtop
ENTRYPOINT ["streamtop"]
