FROM rust:1.85-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/databeam /app/databeam
COPY --from=builder /app/web /app/web
EXPOSE 8080
CMD ["/app/databeam"]
