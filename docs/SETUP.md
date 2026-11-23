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