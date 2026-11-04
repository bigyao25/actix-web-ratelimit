FROM rust:1.98.0-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS build
RUN apk add --no-cache musl-dev
WORKDIR /source
COPY . .
ARG VARIANT=head
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/target CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/target cargo build --release --locked --manifest-path benchmarks/adapters/${VARIANT}/Cargo.toml && cp /target/release/bench-server /bench-server
FROM alpine:3.23@sha256:ff7f0c727dd796aa36787bfcd9a64bf35a6ce72a73451b3359fde849b6703d63
COPY --from=build /bench-server /usr/local/bin/bench-server
USER 65534
ENTRYPOINT ["bench-server"]
