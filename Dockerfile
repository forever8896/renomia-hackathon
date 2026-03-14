FROM rust:1.86-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /app/target/release/renomia-hack /server
EXPOSE 8080
CMD ["/server"]
