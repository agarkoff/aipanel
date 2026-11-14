# syntax=docker/dockerfile:1
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p aipanel-server \
    && cp target/release/aipanel-server /aipanel-server

# Статически слинкованный бинарник, в образе больше ничего нет.
FROM scratch
COPY --from=build /aipanel-server /aipanel-server
USER 65534:65534
EXPOSE 8080
ENTRYPOINT ["/aipanel-server"]
