FROM rust:1.99-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY .cargo ./.cargo
COPY rust ./rust
RUN cargo build --manifest-path rust/Cargo.toml --release --locked -p voiceprompt-cloud --bin voice-recorder

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system app && useradd --system --gid app --uid 10001 app
COPY --from=build /build/rust/target/release/voice-recorder /usr/local/bin/voice-recorder
ENV VR_ENVIRONMENT=production
USER 10001:10001
EXPOSE 8000
ENTRYPOINT ["voice-recorder"]
CMD ["api", "--host", "0.0.0.0", "--port", "8000"]
