FROM rust:1.94 AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd --system --create-home --uid 10001 appuser
WORKDIR /app

COPY --from=build /app/target/release/hashavatar-api /usr/local/bin/hashavatar-api

ENV PORT=8080
EXPOSE 8080
USER appuser
CMD ["hashavatar-api"]
