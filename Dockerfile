# syntax=docker/dockerfile:1

# --- Build: the edgebox binary and the example functions ---
FROM rust:1-bookworm AS build
RUN rustup target add wasm32-wasip2
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p edgebox && cargo guests --locked

# --- Run: a slim image with the binary and the example .wasm files ---
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home edgebox \
 && mkdir /data && chown edgebox /data
COPY --from=build /src/target/release/edgebox /usr/local/bin/edgebox
COPY --from=build /src/target/wasm32-wasip2/release/*.wasm /opt/edgebox/examples/
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh

# Starts as root only to fix volume ownership; edgebox itself runs unprivileged.
WORKDIR /data
VOLUME /data
ENV EDGEBOX_ADDR=0.0.0.0:8080 \
    EDGEBOX_DATA=/data/edgebox.db
EXPOSE 8080
ENTRYPOINT ["entrypoint.sh"]
CMD ["serve"]
