# syntax=docker/dockerfile:1

# Build stage. Multi-arch clean: buildx sets TARGETPLATFORM, so no architecture
# is hard-coded here.
FROM rust:1.92-bookworm AS builder

# Build dependencies for the vertex cone:
# - protobuf-compiler: the gRPC server stack (prost/tonic) generates code from
#   .proto files at build time.
# - cmake, clang, nasm: aws-lc-sys (the rustls crypto backend) builds C and
#   assembly; nasm is needed for the x86_64 assembly paths.
# - pkg-config: dependency discovery.
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    cmake \
    clang \
    nasm \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY . .

# Build the default bare client. BuildKit cache mounts speed up the registry,
# git, and target directories; the binary is copied out within the same layer
# because the target cache mount does not persist to later stages.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release -p vertex && \
    cp target/release/vertex /usr/local/bin/vertex

# Runtime stage.
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root user.
RUN useradd -m -u 1000 vertex
USER vertex
WORKDIR /home/vertex

COPY --from=builder /usr/local/bin/vertex /usr/local/bin/vertex

# p2p and gRPC.
EXPOSE 1634 1635

ENTRYPOINT ["vertex"]
