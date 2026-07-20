FROM rust:1.97@sha256:9a2cd304a852f05d3352f75bc2775242371c0169a72dbb40d5d881379d571989 AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY config ./config
COPY src ./src

RUN cargo build --release \
    && cp target/release/hashavatar-api /usr/local/bin/hashavatar-api \
    && rm -rf target /usr/local/cargo/registry /usr/local/cargo/git

FROM cgr.dev/chainguard/wolfi-base:latest@sha256:02dab76bd852a70556b5b2002195c8a5fdab77d323c433bf6642aab080489795
RUN addgroup -S appuser \
    && adduser -S -D -H -u 10001 -G appuser appuser
WORKDIR /app

COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /usr/local/bin/hashavatar-api /usr/local/bin/hashavatar-api

ENV PORT=8080
ENV PUBLIC_WEBSITE_HOST=0.0.0.0
EXPOSE 8080
USER appuser
CMD ["hashavatar-api"]
