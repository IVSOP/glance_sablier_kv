# syntax=docker/dockerfile:1

FROM rust:slim-trixie AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim AS runtime
WORKDIR /app

COPY --from=build /app/target/release/sablier_mini_kv /usr/local/bin/sablier_mini_kv

ENV LISTEN_ADDR=0.0.0.0:3000 \
    RUST_LOG=info
USER 65532:65532
EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/sablier_mini_kv"]
