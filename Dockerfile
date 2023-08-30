FROM rust:alpine3.18 as builder

WORKDIR /app
RUN apk add musl-dev

COPY . .

# target alpine linux
RUN cargo install --path . --target=x86_64-unknown-linux-musl

# second stage
FROM alpine:3.18
WORKDIR /app
# copy over binary from first stage
COPY --from=builder /usr/local/cargo/bin/fork-observer /usr/local/bin/
COPY --from=builder /app/www ./www/

CMD /usr/local/bin/fork-observer
