FROM rust:latest AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/ironmem /usr/local/bin/ironmem

ENV IRONMEM_MCP_TRANSPORT=sse

EXPOSE 37778 37779

ENTRYPOINT ["ironmem", "server"]
