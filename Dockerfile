# Multi-stage build producing three images:
#   --target backend   Rust node, market simulator service and bulk loader (Kubernetes)
#   --target auth      identity microservice (OAuth 2.1 / OIDC)
#   --target frontend  nginx serving the React/WASM UI with strict security headers (Kubernetes)

FROM rust:1-bookworm AS rust
RUN rustup target add wasm32-unknown-unknown && cargo install wasm-pack --locked
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY backend backend
COPY auth auth
COPY proto proto
RUN cargo build --release -p chaindeal-backend --bins && cargo build --release -p chaindeal-auth
RUN wasm-pack build crates/wallet-wasm --target web --release --out-dir /wasm --out-name chaindeal_wallet

FROM node:20-bookworm-slim AS web
WORKDIR /web
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend ./
COPY --from=rust /wasm ./src/wasm
# Never ship demo private keys, even if present in the build context.
RUN rm -f public/demo-wallets.json && npm run build && rm -f dist/demo-wallets.json

FROM debian:bookworm-slim AS backend
RUN useradd --system --uid 10001 --no-create-home chaindeal
COPY --from=rust /src/target/release/chaindeal-backend /src/target/release/chaindeal-sim /src/target/release/chaindeal-bulk /usr/local/bin/
ENV BIND_ADDR=0.0.0.0:8080
USER 10001
EXPOSE 8080 9090
CMD ["chaindeal-backend"]

FROM debian:bookworm-slim AS auth
RUN useradd --system --uid 10001 --no-create-home chaindeal
COPY --from=rust /src/target/release/chaindeal-auth /usr/local/bin/
ENV BIND_ADDR=0.0.0.0:8080
USER 10001
EXPOSE 8080
CMD ["chaindeal-auth"]

FROM nginxinc/nginx-unprivileged:1.29-alpine AS frontend
COPY deploy/nginx/default.conf /etc/nginx/conf.d/default.conf
COPY deploy/nginx/security-headers.conf /etc/nginx/security-headers.conf
COPY --from=web /web/dist /usr/share/nginx/html
EXPOSE 8080

