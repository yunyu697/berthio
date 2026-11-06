# Linux 构建 + 迁移实跑镜像（M5）。构建产物同时作为 1.0.0 的 Linux 附件来源。
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl jq \
  && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/berthio /usr/local/bin/berthio
ENTRYPOINT ["/bin/sh"]
