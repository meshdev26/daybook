# Deployment Guide

## Mount under an existing reverse proxy (subpath)

Set `WEB_BASE_PATH=/agent` in `.env`. The dashboard and API then live under
that path — every redirect, cookie, and form action is prefixed — so an
existing nginx/caddy serving other apps can add it without conflicts:

```nginx
# nginx — no path rewriting, pass through as-is
location /agent/ {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

```caddy
# caddy
agent.example.com {
    reverse_proxy /agent/* 127.0.0.1:8080
}
```

Notes: `https://host/agent/api/v1/webhooks/cryptopay` is the webhook URL to
register with Crypto Pay; do not strip the prefix at the proxy; the root `/`
is intentionally not served when a base path is set. Also set
`WEB_BASE_PATH` **after** first-run setup or re-add it afterwards — the setup
form rewrites `.env` with a fixed key list (known issue, tracked).

## MFA recovery codes

Recovery codes are 10 one-time codes drawn from the BIP-39 English wordlist and
stored bcrypt-hashed. `MFA_RECOVERY_CODES_ENABLED` defaults to on; `0`, `false`,
`no` or `off` reject stored codes and remove the recovery UI and route from the
dashboard. TOTP enrollment and verification stay available. Stored codes remain
in the database and work again if the flag is turned back on. Confirm that every
privileged account has a working authenticator before disabling the feature.

## Embed into a Rust host

The crate is a library: an existing axum service can mount the whole app:

```rust
let app = daybook::embed::AgentAppBuilder::new()
    .base_path("/agent")
    .db_path("data/host.db")
    .build()
    .await?;
let host = axum::Router::new()
    .route("/", axum::routing::get(|| async { "host" }))
    .merge(app.router());
```

Custom bot commands register without touching the core (`examples/plugin_command.rs`):

```rust
builder.command(CommandSpec {
    name: "echo".into(),
    description: "Echo text back".into(),
    min_role: roles::Role::User,
    handler: Arc::new(|ctx| Box::pin(async move { /* ... */ })),
});
```

## Systemd
```ini
[Unit]
Description=Telegram Business Platform
After=network.target

[Service]
Type=simple
User=telegram
WorkingDirectory=/opt/daybook
EnvironmentFile=/opt/daybook/.env
ExecStart=/opt/daybook/daybook
Restart=always

[Install]
WantedBy=multi-user.target
```

## Docker
```dockerfile
FROM rust:1.98 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/daybook /usr/local/bin/
CMD ["daybook"]
```

For a narrow install, build with Cargo features (e.g. `cargo build --release
--no-default-features --features movie_bot`); the default keeps `shop_bot` and
`moderation`. If moderation ships, set at least one owner floor term before
enabling enforcement — otherwise enforcement is degraded and matching messages
are held for review. See [docs/MULTIBOT_FOUNDATION.md](MULTIBOT_FOUNDATION.md).

## Backups
- Back up `data/agent.db` regularly.
- Store `.env` securely.