# Requested toolchain/image tags; image availability and builds are unverified.
FROM docker.io/library/rust:1.99.1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
# "neural" adds the self-trained byte-level engine (candle, CPU only); set
# FEATURES="" for a smaller statistical-only build.
ARG FEATURES=neural
RUN cargo build --locked --release -p aster-api --features "$FEATURES"

FROM docker.io/library/debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 aster \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin aster
WORKDIR /app
# The runtime writes data/logs and rewrites aster.toml on /set; let it own /app.
RUN mkdir -p /app/data/logs /app/data/ingest && chown -R 10001:10001 /app
# Migrations are embedded in the binary; configuration/secrets stay outside it.
COPY --from=builder /build/target/release/aster /usr/local/bin/aster
USER 10001:10001
EXPOSE 3000
ENTRYPOINT ["aster"]
CMD ["serve"]