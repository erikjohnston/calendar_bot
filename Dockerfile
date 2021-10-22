FROM rust:latest as builder

WORKDIR /build
COPY src src
COPY Cargo.toml .
COPY Cargo.lock .

RUN cargo install --path .

FROM debian:buster-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/calendar_bot /usr/local/bin/calendar_bot

WORKDIR /app

COPY res /app/res

CMD ["calendar_bot"]
