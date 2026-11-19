# syntax=docker/dockerfile:1

FROM rust:slim-trixie AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app

COPY --from=build /app/target/release/sablier_mini_kv /usr/local/bin/sablier_mini_kv

ENV LISTEN_ADDR=0.0.0.0:3000 \
    RUST_LOG=info
USER 65532:65532
EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/sablier_mini_kv"]
