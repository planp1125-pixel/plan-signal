FROM rust:1.77-slim as builder
WORKDIR /app
COPY . .
# Install dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Fix Render Free Tier OOM by limiting memory usage during build
ENV CARGO_BUILD_JOBS=1
ENV CARGO_PROFILE_RELEASE_LTO="false"
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256

RUN cargo build --release --jobs 1

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates sqlite3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/plan-signal .

# Render defaults to /data for persistent disks, but /tmp works for ephemeral testing
ENV DATABASE_PATH=/tmp/plan_signal.db
ENV PORT=3000
EXPOSE 3000

CMD ["./plan-signal"]
