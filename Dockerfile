FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates sqlite3 && rm -rf /var/lib/apt/lists/*
COPY plan-signal-bin ./plan-signal
RUN chmod +x ./plan-signal
ENV DATABASE_PATH=/tmp/plan_signal.db
ENV PORT=3000
EXPOSE 3000
CMD ["./plan-signal"]
