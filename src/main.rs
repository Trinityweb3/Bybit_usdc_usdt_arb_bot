mod bybit;
mod telegram;

use anyhow::{anyhow, Result};
use bybit::{floor_to_step, format_qty, BybitClient, InstrumentInfo};
use std::collections::HashSet;
use std::env;
use std::time::Duration;
use tracing::{error, info, warn};

/// ---- Configuration (override via env vars) ----------------------------
struct Config {
    api_key: String,
    api_secret: String,
    testnet: bool,
    dry_run: bool,
    leverage: u32,
    notional_usdt: f64,
    notional_usdc: f64,
    entry_spread_pct: f64, // e.g. 0.004 = 0.4%
    poll_interval_secs: u64,
    telegram_bot_token: String,
    telegram_chat_id: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        let api_key = env::var("BYBIT_API_KEY")
            .map_err(|_| anyhow!("BYBIT_API_KEY env var is not set"))?;
        let api_secret = env::var("BYBIT_API_SECRET")
            .map_err(|_| anyhow!("BYBIT_API_SECRET env var is not set"))?;
        let telegram_bot_token = env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| anyhow!("TELEGRAM_BOT_TOKEN env var is not set"))?;
        let telegram_chat_id = env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| anyhow!("TELEGRAM_CHAT_ID env var is not set"))?;

        Ok(Self {
            api_key,
            api_secret,
            testnet: env::var("BYBIT_TESTNET").map(|v| v == "true").unwrap_or(false),
            dry_run: env::var("DRY_RUN").map(|v| v != "false").unwrap_or(true),
            leverage: env::var("LEVERAGE").ok().and_then(|v| v.parse().ok()).unwrap_or(10),
            notional_usdt: env::var("NOTIONAL_USDT").ok().and_then(|v| v.parse().ok()).unwrap_or(100.0),
            notional_usdc: env::var("NOTIONAL_USDC").ok().and_then(|v| v.parse().ok()).unwrap_or(100.0),
            entry_spread_pct: env::var("ENTRY_SPREAD_PCT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.004),
            poll_interval_secs: env::var("POLL_INTERVAL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(3),
            telegram_bot_token,
            telegram_chat_id,
        })
    }
}

/// A base coin that has both a USDT-margined and USDC-margined perpetual.
struct Pair {
    base_coin: String,
    usdt: InstrumentInfo,
    usdc: InstrumentInfo,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Loads variables from a .env file in the current directory into the
    // process environment, if one exists. Silently does nothing if there's
    // no .env file - real environment variables always take priority.
    let _ = dotenvy::dotenv();

    // Guarantees INFO-level logs (order placements, discovered spreads, etc.)
    // always show up in the terminal, even if RUST_LOG isn't set. Set
    // RUST_LOG=debug (or similar) to see more detail.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cfg = Config::from_env()?;
    if cfg.dry_run {
        warn!("Running in DRY_RUN mode - no real orders and no Telegram messages will be sent. Set DRY_RUN=false to trade live.");
    }

    let client = BybitClient::new(cfg.api_key.clone(), cfg.api_secret.clone(), cfg.testnet);
    let http = reqwest::Client::new();

    if cfg.dry_run {
        info!("[DRY_RUN] Skipping Telegram config verification (would verify in live mode)");
    } else {
        match telegram::verify_chat(&http, &cfg.telegram_bot_token, &cfg.telegram_chat_id).await {
            Ok(_) => info!("Telegram config OK - bot can message TELEGRAM_CHAT_ID={}", cfg.telegram_chat_id),
            Err(e) => warn!(
                "Telegram config check failed ({e:#}) - \"/gm\" messages will fail after trades. \
                 Double-check TELEGRAM_CHAT_ID and that you've messaged the bot at least once."
            ),
        }
    }

    info!("Discovering symbols that have both USDT and USDC perpetuals...");
    let pairs = discover_pairs(&client).await?;
    info!("Found {} base coins tradable against both USDT and USDC", pairs.len());

    // Base coins we've already entered - the bot only opens once per coin
    // and never manages an exit, so we never remove entries from this set.
    let mut entered: HashSet<String> = HashSet::new();

    loop {
        match run_cycle(&client, &http, &cfg, &pairs, &mut entered).await {
            Ok(_) => {}
            Err(e) => error!("Cycle error: {e:#}"),
        }
        tokio::time::sleep(Duration::from_secs(cfg.poll_interval_secs)).await;
    }
}

/// Fetches all linear instruments and groups them by base coin, keeping
/// only the ones that trade against both USDT and USDC.
async fn discover_pairs(client: &BybitClient) -> Result<Vec<Pair>> {
    let instruments = client.get_all_linear_instruments().await?;

    let mut usdt_map: std::collections::HashMap<String, InstrumentInfo> = std::collections::HashMap::new();
    let mut usdc_map: std::collections::HashMap<String, InstrumentInfo> = std::collections::HashMap::new();

    for inst in instruments {
        if inst.status != "Trading" {
            continue;
        }
        match inst.quote_coin.as_str() {
            "USDT" => {
                usdt_map.insert(inst.base_coin.clone(), inst);
            }
            "USDC" => {
                usdc_map.insert(inst.base_coin.clone(), inst);
            }
            _ => {}
        }
    }

    let mut pairs = Vec::new();
    for (base_coin, usdt_inst) in usdt_map.into_iter() {
        if let Some(usdc_inst) = usdc_map.remove(&base_coin) {
            pairs.push(Pair {
                base_coin,
                usdt: usdt_inst,
                usdc: usdc_inst,
            });
        }
    }
    Ok(pairs)
}

async fn run_cycle(
    client: &BybitClient,
    http: &reqwest::Client,
    cfg: &Config,
    pairs: &[Pair],
    entered: &mut HashSet<String>,
) -> Result<()> {
    let tickers = client.get_all_tickers().await?;

    for pair in pairs {
        if entered.contains(&pair.base_coin) {
            continue;
        }

        let usdt_ticker = match tickers.get(&pair.usdt.symbol) {
            Some(t) => t,
            None => continue,
        };
        let usdc_ticker = match tickers.get(&pair.usdc.symbol) {
            Some(t) => t,
            None => continue,
        };

        // Two possible directions: buy cheap leg, sell expensive leg.
        // Spread A: sell USDT-perp (bid), buy USDC-perp (ask)
        let spread_a = (usdt_ticker.bid1_price - usdc_ticker.ask1_price) / usdc_ticker.ask1_price;
        // Spread B: sell USDC-perp (bid), buy USDT-perp (ask)
        let spread_b = (usdc_ticker.bid1_price - usdt_ticker.ask1_price) / usdt_ticker.ask1_price;

        let (direction, spread, long_symbol, short_symbol) = if spread_a > spread_b {
            ("A", spread_a, pair.usdc.symbol.clone(), pair.usdt.symbol.clone())
        } else {
            ("B", spread_b, pair.usdt.symbol.clone(), pair.usdc.symbol.clone())
        };

        if spread <= cfg.entry_spread_pct {
            continue;
        }

        info!(
            "{}: spread {} = {:.4}% exceeds entry threshold - opening position (long {}, short {})",
            pair.base_coin,
            direction,
            spread * 100.0,
            long_symbol,
            short_symbol
        );

        match open_position(client, cfg, pair, &long_symbol, &short_symbol, usdt_ticker, usdc_ticker).await {
            Ok(_qty) => {
                entered.insert(pair.base_coin.clone());

                if cfg.dry_run {
                    info!("[DRY_RUN] Would send Telegram message \"/gm\"");
                } else {
                    match telegram::send_message(http, &cfg.telegram_bot_token, &cfg.telegram_chat_id, "/gm").await {
                        Ok(_) => info!("{}: sent \"/gm\" to Telegram", pair.base_coin),
                        Err(e) => error!("{}: failed to send Telegram message: {e:#}", pair.base_coin),
                    }
                }
            }
            Err(e) => error!("{}: failed to open position: {e:#}", pair.base_coin),
        }
    }

    Ok(())
}

async fn open_position(
    client: &BybitClient,
    cfg: &Config,
    pair: &Pair,
    long_symbol: &str,
    short_symbol: &str,
    usdt_ticker: &bybit::Ticker,
    usdc_ticker: &bybit::Ticker,
) -> Result<String> {
    // Size the position: same token quantity on both legs, targeting
    // ~notional_usdt / ~notional_usdc respectively. We take the more
    // conservative (smaller) quantity of the two, then round down to
    // whichever leg has the coarser qty step so both orders are valid.
    let qty_from_usdt = cfg.notional_usdt / usdt_ticker.ask1_price.max(usdt_ticker.bid1_price);
    let qty_from_usdc = cfg.notional_usdc / usdc_ticker.ask1_price.max(usdc_ticker.bid1_price);
    let raw_qty = qty_from_usdt.min(qty_from_usdc);

    let usdt_step: f64 = pair.usdt.lot_size_filter.qty_step.parse().unwrap_or(1.0);
    let usdc_step: f64 = pair.usdc.lot_size_filter.qty_step.parse().unwrap_or(1.0);
    let coarser_step_str = if usdt_step >= usdc_step {
        &pair.usdt.lot_size_filter.qty_step
    } else {
        &pair.usdc.lot_size_filter.qty_step
    };
    let step: f64 = coarser_step_str.parse().unwrap_or(1.0);

    // Bybit enforces a minimum order VALUE (commonly ~$5 notional) per leg,
    // on top of the lot-size minimum qty. Flooring the qty to the step can
    // push the resulting notional just under that floor even when the
    // configured NOTIONAL_USDT/NOTIONAL_USDC was above it, so if that
    // happens we try rounding up one step instead before giving up.
    const MIN_ORDER_VALUE_BUFFER: f64 = 5.5; // small buffer above Bybit's ~$5 floor

    let notional_at = |qty: f64| -> (f64, f64) {
        (
            qty * usdt_ticker.ask1_price.max(usdt_ticker.bid1_price),
            qty * usdc_ticker.ask1_price.max(usdc_ticker.bid1_price),
        )
    };

    let mut qty_f = floor_to_step(raw_qty, coarser_step_str);
    let (mut notional_usdt, mut notional_usdc) = notional_at(qty_f);

    if notional_usdt < MIN_ORDER_VALUE_BUFFER || notional_usdc < MIN_ORDER_VALUE_BUFFER {
        qty_f += step;
        let (nu, nc) = notional_at(qty_f);
        notional_usdt = nu;
        notional_usdc = nc;
    }

    let qty_str = format_qty(qty_f, coarser_step_str);

    let min_usdt: f64 = pair.usdt.lot_size_filter.min_order_qty.parse().unwrap_or(0.0);
    let min_usdc: f64 = pair.usdc.lot_size_filter.min_order_qty.parse().unwrap_or(0.0);
    if qty_f <= 0.0 || qty_f < min_usdt || qty_f < min_usdc {
        return Err(anyhow!(
            "computed qty {} is below the lot-size minimum (usdt min {}, usdc min {}) - raise NOTIONAL_USDT/NOTIONAL_USDC",
            qty_str,
            min_usdt,
            min_usdc
        ));
    }

    if notional_usdt < MIN_ORDER_VALUE_BUFFER || notional_usdc < MIN_ORDER_VALUE_BUFFER {
        return Err(anyhow!(
            "even rounded up, notional (${:.2} USDT-leg / ${:.2} USDC-leg) is still below Bybit's ~$5 minimum order value - raise NOTIONAL_USDT/NOTIONAL_USDC",
            notional_usdt,
            notional_usdc
        ));
    }

    if cfg.dry_run {
        info!(
            "[DRY_RUN] Would set {}x leverage and open long {} / short {} for qty {} (~${:.2} USDT-leg / ~${:.2} USDC-leg)",
            cfg.leverage, long_symbol, short_symbol, qty_str, notional_usdt, notional_usdc
        );
        return Ok(qty_str);
    }

    client.set_leverage(&pair.usdt.symbol, cfg.leverage).await?;
    client.set_leverage(&pair.usdc.symbol, cfg.leverage).await?;

    let long_result = client.place_market_order(long_symbol, "Buy", &qty_str, false).await;
    let short_result = client.place_market_order(short_symbol, "Sell", &qty_str, false).await;

    match (&long_result, &short_result) {
        (Ok(_), Ok(_)) => Ok(qty_str),
        (Ok(_), Err(e)) => {
            error!("Short leg failed ({e:#}); unwinding long leg to avoid a naked position");
            let _ = client.place_market_order(long_symbol, "Sell", &qty_str, true).await;
            Err(anyhow!("short leg failed, long leg unwound: {e:#}"))
        }
        (Err(e), Ok(_)) => {
            error!("Long leg failed ({e:#}); unwinding short leg to avoid a naked position");
            let _ = client.place_market_order(short_symbol, "Buy", &qty_str, true).await;
            Err(anyhow!("long leg failed, short leg unwound: {e:#}"))
        }
        (Err(e1), Err(e2)) => Err(anyhow!("both legs failed: long={e1:#}, short={e2:#}")),
    }
}