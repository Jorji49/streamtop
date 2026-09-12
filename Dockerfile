# Multi-stage build: compile on Alpine musl, run on minimal Alpine.
FROM rust:1.98-alpine@sha256:1716b3aa042d735f4566d14dc54e8037de9d69556e2d5dd58131d93a613d173d AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY src ./src
COPY tests ./tests
COPY LICENSE README.md CHANGELOG.md ./

RUN cargo build --release --locked \
    && strip target/release/streamtop

FROM alpine:3.22@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce

RUN apk add --no-cache ca-certificates libgcc \
    && adduser -D -H -u 10001 streamtop

COPY --from=builder /app/target/release/streamtop /usr/local/bin/streamtop

USER streamtop
ENTRYPOINT ["streamtop"]
