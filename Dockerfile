# Requested toolchain/image tags; image availability and builds are unverified.
FROM rust:1.99.1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --locked --release -p aster-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 aster \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin aster
WORKDIR /app
# Migrations are embedded in the binary; configuration/secrets stay outside it.
COPY --from=builder /build/target/release/aster /usr/local/bin/aster
USER 10001:10001
EXPOSE 3000
ENTRYPOINT ["aster"]
CMD ["serve"]