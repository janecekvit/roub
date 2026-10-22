# roub-server on Debian. Build: docker build -t roub-server .
# Keep the Rust version in sync with rust-toolchain.toml.
FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p roub-server

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home roub \
    && mkdir -p /var/lib/roub/blobs \
    && chown roub /var/lib/roub/blobs
COPY --from=build /src/target/release/roub-server /usr/local/bin/roub-server
USER roub
ENV ROUB_LISTEN=0.0.0.0:7470
# Used together with ROUB_DATABASE_URL; mount a volume here.
ENV ROUB_BLOB_DIR=/var/lib/roub/blobs
VOLUME /var/lib/roub/blobs
EXPOSE 7470
ENTRYPOINT ["roub-server"]
