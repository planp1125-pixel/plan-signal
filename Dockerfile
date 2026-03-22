FROM rust:1.77-slim as builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/plan-signal .
ENV DATABASE_PATH=/data/plan_signal.db
ENV PORT=3000
VOLUME ["/data"]
EXPOSE 3000
CMD ["./plan-signal"]
