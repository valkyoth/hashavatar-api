FROM rust:1.94 as build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY public-website/Cargo.toml ./public-website/Cargo.toml
COPY public-website/src ./public-website/src

WORKDIR /app/public-website
RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd --system --create-home appuser
WORKDIR /app
COPY --from=build /app/public-website/target/release/hashavatar-api /usr/local/bin/hashavatar-api
USER appuser
EXPOSE 8080
CMD ["hashavatar-api"]
