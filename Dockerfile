FROM rust:1.97.1@sha256:4f9fcd47f7c1126d2c8dd20e594f8ec852492fe429c2c6c11ca56eebfffaf0d9 AS build
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY config ./config
COPY src ./src

RUN cargo build --release \
    && cp target/release/hashavatar-website /usr/local/bin/hashavatar-website \
    && rm -rf target /usr/local/cargo/registry /usr/local/cargo/git

FROM cgr.dev/chainguard/wolfi-base:latest@sha256:02dab76bd852a70556b5b2002195c8a5fdab77d323c433bf6642aab080489795
RUN addgroup -S appuser \
    && adduser -S -D -H -u 10001 -G appuser appuser
WORKDIR /app

COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /usr/local/bin/hashavatar-website /usr/local/bin/hashavatar-website

ENV PORT=8080
ENV PUBLIC_WEBSITE_HOST=0.0.0.0
EXPOSE 8080
USER appuser
CMD ["hashavatar-website"]
