# Build stage
FROM rust:1.94 as builder
WORKDIR /usr/src/syncpond-server

# Copy manifests, source, and docs into build context
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY doc ./doc

# Build dependencies and application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Copy binary from builder
COPY --from=builder /usr/src/syncpond-server/target/release/syncpond-server ./syncpond-server

EXPOSE 8080 9090

ENTRYPOINT ["./syncpond-server"]
