# Multi-stage build: compile on Alpine musl, run on minimal Alpine.
FROM rust:1.98-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
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
