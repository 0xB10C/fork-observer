# Two-stage build to reduce final image size
# for more info see: https://docs.docker.com/build/building/multi-stage/

# First build stage
FROM rust:1.67 as builder
WORKDIR /usr/src/fork-observer
COPY . .
RUN cargo install --path .

# Second build stage
FROM debian:bullseye-slim
WORKDIR /fork-observer
COPY --from=builder /usr/local/cargo/bin/fork-observer /fork-observer/fork-observer
COPY --from=builder /usr/src/fork-observer/config.toml.example /fork-observer/config.toml
COPY --from=builder /usr/src/fork-observer/www /fork-observer/www/
ENV CONFIG_FILE=/fork-observer/config.toml
CMD ["/fork-observer/fork-observer"]
