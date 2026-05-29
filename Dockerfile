FROM rust:1.96 AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release \
    && cp target/release/hashavatar-api /usr/local/bin/hashavatar-api \
    && rm -rf target /usr/local/cargo/registry /usr/local/cargo/git

FROM cgr.dev/chainguard/wolfi-base:latest
RUN apk add --no-cache ca-certificates glibc \
    && addgroup -S appuser \
    && adduser -S -D -H -u 10001 -G appuser appuser
WORKDIR /app

COPY --from=build /usr/local/bin/hashavatar-api /usr/local/bin/hashavatar-api

ENV PORT=8080
ENV PUBLIC_WEBSITE_HOST=0.0.0.0
EXPOSE 8080
USER appuser
CMD ["hashavatar-api"]
