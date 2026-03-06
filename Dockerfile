# syntax=docker/dockerfile:1

FROM rust:1.86-bookworm AS builder
WORKDIR /app

COPY . .
RUN cargo build --release -p remoterm-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        tini \
        zsh \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
RUN mkdir -p /data /workspace

COPY --from=builder /app/target/release/remoterm-server /usr/local/bin/remoterm-server

ENV RUST_LOG=remoterm_server=info

EXPOSE 8787

ENTRYPOINT ["/usr/bin/tini", "--", "remoterm-server"]
CMD ["--listen", "0.0.0.0:8787", "--db-path", "/data/remoterm.sqlite3"]
