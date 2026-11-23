# Setup Guide

## 1. Create the bot
1. Open Telegram, chat with @BotFather.
2. Send `/newbot`, follow prompts, get token.
3. Add the bot to your target channel as **administrator** with post/pin/invite/ban rights.

## 2. Get your user ID
Chat with @userinfobot to get your numeric user ID.

## 3. Configure
```bash
cp .env.example .env
# edit .env
```
Set `TELEGRAM_BOT_TOKEN` and `OWNER_ID`. Optionally `DEFAULT_CHANNEL` and `CRYPTO_PAY_TOKEN`. `MFA_RECOVERY_CODES_ENABLED` defaults to on; `0`, `false`, `no` or `off` disables MFA recovery codes while TOTP sign-in continues to work.

To store secrets in the web console (currently the Crypto Pay token), also set `MASTER_KEY` to 32 bytes of key material, encoded as either 64 hexadecimal characters or standard base64 (e.g. generate with `openssl rand -hex 32`). A set-but-invalid key aborts startup. Without `MASTER_KEY`, secret writes are refused with an explicit console error; existing plaintext tokens remain readable but should be re-saved once to encrypt them at rest. Keep a backup of the key offline: changing it makes previously encrypted secrets unreadable.

## 4. Build
```bash
cargo build --release
```

## 5. Run
```bash
./target/release/daybook
```

## 6. First use
- `/setupbot` — configure bot profile.
- `/setchannel @yourchannel`
- `/info` — verify.

## 7. Crypto Pay (optional)
1. Open @CryptoBot → Crypto Pay → Create App → get token.
2. Set the token in the web console's `/crypto` page or via `CRYPTO_PAY_TOKEN`.
3. Create plans and sell.

## 8. Periodic expiry
Expiry is automatic: the in-process 6h sweeper revokes expired access (the 30s loop verifies invoice payments). `/expirecheck` is available as a manual trigger only; no cron is required.