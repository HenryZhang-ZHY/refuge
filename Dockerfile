FROM node:26-bookworm-slim AS web-builder

WORKDIR /build/web
RUN npm install --global pnpm@12.4.1
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

FROM rust:1.94-bookworm AS rust-builder

WORKDIR /build/refuge
COPY . .
COPY --from=web-builder /build/web/dist ./web/dist
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 10001 refuge \
    && useradd --system --uid 10001 --gid 10001 --no-create-home --home-dir /var/lib/refuge refuge \
    && install --directory --mode 0750 --owner refuge --group refuge /var/lib/refuge \
    && test "$(stat --format '%u:%g' /var/lib/refuge)" = "10001:10001" \
    && test -z "$(find /var/lib/refuge -mindepth 1 -maxdepth 1 -print -quit)"

COPY --from=rust-builder /build/refuge/target/release/refuge /usr/local/bin/refuge

VOLUME ["/var/lib/refuge"]
EXPOSE 7788
USER refuge

ENTRYPOINT ["refuge"]
CMD ["serve", "--data", "/var/lib/refuge", "--backup", "/var/backups/refuge", "--listen", "0.0.0.0:7788"]
