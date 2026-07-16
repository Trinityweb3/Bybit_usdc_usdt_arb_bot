# Bybit USDT/USDC Perp Spread Bot

This bot scans all linear perpetual contracts on Bybit, looks for coins that have
both `XXXUSDT` and `XXXUSDC` contracts available, and whenever a spread appears
between them, it opens a long position on one leg and a short position on the
other using the same token quantity (roughly $100 notional on each side by
default, with 10x leverage).

**The bot only opens positions.** It does not close them automatically. Once it
finds a spread, opens both legs, and sends a `/gm` message to Telegram (For Pushover), it
considers that coin done and never touches it again during that session.

Position management — exits, stop losses, profit taking — is entirely up to you.

## How it works

1. On startup, the bot calls `GET /v5/market/instruments-info` once to find all
   base assets that have both USDT and USDC perpetual contracts.

2. Every `POLL_INTERVAL_SECS` seconds, it fetches bid/ask prices for all symbols
   in a single request using `GET /v5/market/tickers`.

3. The spread is calculated using executable prices rather than mid prices:

   `(bid on one leg - ask on the other leg) / ask`

   The calculation is performed in both directions.

4. If the spread exceeds `ENTRY_SPREAD_PCT` and the bot hasn't already entered a
   trade for that coin, it sets leverage and simultaneously places:

   * a Market buy order on the cheaper leg
   * a Market sell order on the more expensive leg

   Both orders use the same token quantity.

5. If one of the two orders fails while the other succeeds, the bot immediately
   closes the successful leg with a reduce-only order to avoid ending up with an
   unhedged position. This is the only situation where the bot closes positions
   on its own.

6. Once both legs are opened successfully, the bot sends `/gm` to the specified
   Telegram chat using the Telegram Bot API and marks that coin as "already
   traded" so it won't repeatedly reopen the same opportunity every polling
   cycle while the spread remains available.

## Running the bot

```bash
BYBIT_API_KEY=xxx
BYBIT_API_SECRET=xxx
BYBIT_TESTNET=true

TELEGRAM_BOT_TOKEN=xxx
TELEGRAM_CHAT_ID=xxx

# true -> logging only, not executing orders and sending messages
DRY_RUN=false
LEVERAGE=10

NOTIONAL_USDT=1000
NOTIONAL_USDC=1000
ENTRY_SPREAD_PCT=0.01 (1%)
POLL_INTERVAL_SECS=3

```

The bot needs to know `TELEGRAM_CHAT_ID` in advance. The easiest way is to send
any message to the bot in a private chat or add it to a group/channel, then
retrieve `chat.id` via:

`https://api.telegram.org/bot<TOKEN>/getUpdates`

## Environment variables

| Variable             | Default | Description                                           |
| -------------------- | ------- | ----------------------------------------------------- |
| `BYBIT_API_KEY`      | —       | Required                                              |
| `BYBIT_API_SECRET`   | —       | Required                                              |
| `TELEGRAM_BOT_TOKEN` | —       | Required                                              |
| `TELEGRAM_CHAT_ID`   | —       | Required                                              |
| `BYBIT_TESTNET`      | `false` | `true` → use `api-testnet.bybit.com`                  |
| `DRY_RUN`            | `true`  | `true` → log only, no orders and no Telegram messages |
| `LEVERAGE`           | `10`    | Leverage used on both legs                            |
| `NOTIONAL_USDT`      | `100`   | Target notional size for the USDT leg                 |
| `NOTIONAL_USDC`      | `100`   | Target notional size for the USDC leg                 |
| `ENTRY_SPREAD_PCT`   | `0.004` | Entry threshold (0.4%)                                |
| `POLL_INTERVAL_SECS` | `3`     | Ticker polling interval in seconds                    |

## Important limitations and risks — read before using real money

* **Positions are not closed automatically.**
  The bot opens trades and forgets about them. Monitoring, exiting, and risk
  management must be handled manually (or by a separate script/bot).

  Keep this in mind when choosing leverage and position size: running 10x
  leverage without a clear exit plan means divergence between the two legs can
  consume your margin faster than you may be able to react.

* **Each coin is traded only once per session.**
  The list of "already traded" coins exists only in memory. If the bot restarts,
  it forgets previous trades and may open a new position even if the old one is
  still active.

* **Fees and funding eat into the spread.**
  Taker fees on both legs already cost roughly 0.1-0.15% on entry, and funding
  rates between USDT and USDC perpetuals for the same asset are often different.

  `ENTRY_SPREAD_PCT = 0.4%` should be treated as a starting point for testing,
  not as a guaranteed source of profit.

* **USDC is not always worth exactly $1.**
  The spread calculation assumes USDC and USDT are interchangeable at par value.
  During a USDC depeg event, this can generate false signals.

* **No websocket support.**
  The bot uses REST polling every few seconds. That may be too slow for spreads
  that converge quickly. For lower latency execution, you'd want to use:

  `wss://stream.bybit.com/v5/public/linear`

* **Orders are sent as Market/IOC orders.**
  Slippage is possible, especially on lower-liquidity altcoins with thin order
  books.

Before trading live, always run the bot with `DRY_RUN=true` (and optionally
`BYBIT_TESTNET=true`) first. Check the logs, verify that the bot is finding
reasonable spreads and opening sensible pairs, and only then switch to
`DRY_RUN=false`.
