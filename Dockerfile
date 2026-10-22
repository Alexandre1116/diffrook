# syntax=docker/dockerfile:1
FROM node:22-bookworm-slim AS web
WORKDIR /build/web
COPY web/package*.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS server
WORKDIR /build
COPY Cargo.toml Cargo.lock LICENSE ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --locked --release && cp target/release/oversight /oversight

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 oversight \
    && useradd --uid 10001 --gid oversight --no-create-home --shell /usr/sbin/nologin oversight \
    && mkdir -p /data /app/web \
    && chown oversight:oversight /data \
    && chmod 700 /data
COPY --from=server /oversight /usr/local/bin/oversight
COPY --from=web /build/web/dist /app/web
COPY LICENSE /app/LICENSE
COPY docs/licenses/ /app/licenses/
ENV OVERSIGHT_DATA_DIR=/data \
    OVERSIGHT_WEB_DIR=/app/web \
    OVERSIGHT_HOST=0.0.0.0 \
    OVERSIGHT_PORT=8080 \
    RUST_LOG=oversight=info,tower_http=warn
USER oversight
WORKDIR /app
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/oversight"]
