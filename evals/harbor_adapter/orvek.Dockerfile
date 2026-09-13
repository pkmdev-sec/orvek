# syntax=docker/dockerfile:1.7

FROM rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900 AS build

ARG CARGO_PROFILE=release
ARG TARGETARCH

WORKDIR /build
RUN apk add --no-cache musl-dev
COPY . .
RUN --mount=type=cache,id=orvek-harbor-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=orvek-harbor-target-${TARGETARCH},target=/build/target \
    cargo build --locked --profile "${CARGO_PROFILE}" --package orvek --bin orvek --features harbor-evals && \
    case "${CARGO_PROFILE}" in \
        dev) artifact_dir=debug ;; \
        *) artifact_dir="${CARGO_PROFILE}" ;; \
    esac && \
    mkdir /out && \
    cp "target/${artifact_dir}/orvek" /out/orvek

FROM scratch AS artifact
COPY --from=build /out/orvek /orvek
