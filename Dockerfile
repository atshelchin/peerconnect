# PeerConnect relay: one static binary in an otherwise empty image.
#
#   docker build -t peerconnect-relay .
#   docker run -p 8080:8080 peerconnect-relay
#
# Any Rust image can build it (e.g. --build-arg RUST_IMAGE=rust:1.97); the
# binary is linked statically either way.

ARG RUST_IMAGE=rust:1.97-alpine
FROM ${RUST_IMAGE} AS build
RUN if command -v apk >/dev/null; then apk add --no-cache musl-dev; fi
WORKDIR /src
COPY . .
# An explicit --target keeps the static-linking flag off build scripts and macros.
ENV RUSTFLAGS="-C target-feature=+crt-static"
RUN target="$(rustc -vV | sed -n 's/^host: //p')" \
 && cargo build --release --locked -p peerconnect-relay --target "$target" \
 && cp "target/$target/release/peerconnect-relay" /peerconnect-relay

FROM scratch
COPY --from=build /peerconnect-relay /peerconnect-relay
ENV PEERCONNECT_LISTEN=0.0.0.0:8080
EXPOSE 8080
USER 65534:65534
HEALTHCHECK --interval=30s --timeout=5s CMD ["/peerconnect-relay", "healthcheck"]
ENTRYPOINT ["/peerconnect-relay"]
