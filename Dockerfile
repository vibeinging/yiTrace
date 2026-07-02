FROM node:20-bookworm-slim AS console
WORKDIR /src/yitrace-console
COPY yitrace-console/package*.json ./
RUN npm config set registry https://registry.npmjs.org/ \
    && npm config set replace-registry-host always \
    && npm ci
COPY yitrace-console/ ./
RUN VITE_API=http npm run build

FROM rust:1.91-bookworm AS engine
WORKDIR /src
COPY yitrace-engine/ ./yitrace-engine/
COPY --from=console /src/yitrace-console/dist ./yitrace-engine/crates/yt-engine/console_dist
WORKDIR /src/yitrace-engine
RUN cargo build --release -p yt-engine --example server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=engine /src/yitrace-engine/target/release/examples/server /usr/local/bin/yitrace-server
ENV YT_BIND=0.0.0.0:7878
EXPOSE 7878
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:7878/v1/healthz >/dev/null || exit 1
CMD ["yitrace-server"]
