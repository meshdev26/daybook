# Quickstart

5-10 minutes to a working bot. Full details: [docs/SETUP.md](docs/SETUP.md).

## Native

```bash
git clone https://github.com/meshdev26/daybook.git && cd daybook
cp .env.example .env
# fill TELEGRAM_BOT_TOKEN, OWNER_ID, WEB_URL; optional CRYPTO_PAY_TOKEN
cargo build --release
./target/release/daybook
```

- `TELEGRAM_BOT_TOKEN` — from @BotFather.
- `OWNER_ID` — your Telegram user ID (@userinfobot).
- `WEB_URL` — seller-reachable dashboard URL; required for `/becomeseller` and `/dashboard` (one-time claim links). Do not use `127.0.0.1`: in a Telegram DM it points at the recipient's machine.
- `CRYPTO_PAY_TOKEN` — optional, from @CryptoBot → Crypto Pay.
- `MFA_RECOVERY_CODES_ENABLED=0` — disables recovery codes; TOTP still works.

Open http://127.0.0.1:8080 and complete the wizard (`WEB_USERNAME` / `WEB_PASSWORD` from `.env`).

In Telegram: `/setup @yourchannel` (add the bot to the channel as admin first).

## Docker

```bash
docker compose up -d --build
```

Reverse proxy and TLS: [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md).

Commands: [docs/COMMANDS.md](docs/COMMANDS.md). Setup details: [docs/SETUP.md](docs/SETUP.md).
