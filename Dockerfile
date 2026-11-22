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
    cargo build --locked --release && cp target/release/diffrook /diffrook
RUN mkdir /runtime-data && chmod 700 /runtime-data && \
    cp "$(gcc -print-file-name=libgcc_s.so.1)" /libgcc_s.so.1

FROM gcr.io/distroless/base-nossl-debian13:nonroot@sha256:89815d620ac01d8de13cdaf04d19ed874447ca2296d4985b14b818f6e5e47854 AS runtime
COPY --from=server --chown=10001:10001 /runtime-data /data
COPY --from=server /diffrook /usr/local/bin/diffrook
COPY --from=server /libgcc_s.so.1 /usr/local/lib/libgcc_s.so.1
COPY --from=web /build/web/dist /app/web
COPY LICENSE /app/LICENSE
COPY docs/licenses/ /app/licenses/
ENV LD_LIBRARY_PATH=/usr/local/lib \
    DIFFROOK_DATA_DIR=/data \
    DIFFROOK_WEB_DIR=/app/web \
    DIFFROOK_HOST=0.0.0.0 \
    DIFFROOK_PORT=8080 \
    RUST_LOG=diffrook=info,tower_http=warn
USER 10001:10001
WORKDIR /app
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["/usr/local/bin/diffrook", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/diffrook"]
