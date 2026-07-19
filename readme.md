# Bybit USDT/USDC Perp Spread Bot

This bot scans all linear perpetual contracts on Bybit, looks for coins that have
both `XXXUSDT` and `XXXUSDC` contracts available, and whenever a spread appears
between them, it opens a long position on one leg and a short position on the
other using the same token quantity (roughly $100 notional on each side by
default, with 10x leverage).

**The bot only opens positions.** It does not close them automatically. Once it
finds a spread, opens both legs, and sends a `/gm` message to Telegram (For Pushover impl), it
considers that coin done and never touches it again during that session.

Position management — exits, stop losses, profit taking — is entirely up to you.

## Running the bot (more in env_guide) 

create  .env in root

```bash
BYBIT_API_KEY=xxx
BYBIT_API_SECRET=xxx
BYBIT_TESTNET=true

TELEGRAM_BOT_TOKEN=xxx
TELEGRAM_CHAT_ID=xxx

DRY_RUN=false
LEVERAGE=10

USDT_SIZE=1000
USDC_SIZE=1000

ENTRY_SPREAD_PCT=0.01 (1%)
POLL_INTERVAL_SECS=3

```

