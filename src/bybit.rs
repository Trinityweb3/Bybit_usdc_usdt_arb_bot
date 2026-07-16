use anyhow::{anyhow, Context, Result};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

type HmacSha256 = Hmac<Sha256>;

const RECV_WINDOW: &str = "5000";

#[derive(Clone)]
pub struct BybitClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    api_secret: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstrumentInfo {
    pub symbol: String,
    #[serde(rename = "baseCoin")]
    pub base_coin: String,
    #[serde(rename = "quoteCoin")]
    pub quote_coin: String,
    pub status: String,
    #[serde(rename = "lotSizeFilter")]
    pub lot_size_filter: LotSizeFilter,
    #[serde(rename = "priceFilter")]
    pub price_filter: PriceFilter,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LotSizeFilter {
    #[serde(rename = "qtyStep")]
    pub qty_step: String,
    #[serde(rename = "minOrderQty")]
    pub min_order_qty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceFilter {
    #[serde(rename = "tickSize")]
    pub tick_size: String,
}

#[derive(Debug, Clone)]
pub struct Ticker {
    pub symbol: String,
    pub bid1_price: f64,
    pub ask1_price: f64,
}

impl BybitClient {
    pub fn new(api_key: String, api_secret: String, testnet: bool) -> Self {
        let base_url = if testnet {
            "https://api-testnet.bybit.com".to_string()
        } else {
            "https://api.bybit.com".to_string()
        };
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
            api_secret,
        }
    }

    fn timestamp_ms() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .to_string()
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Signed GET request. `params` preserves insertion order and is used
    /// both to build the query string and to compute the signature.
    async fn get_signed(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let query_string = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");

        let timestamp = Self::timestamp_ms();
        let payload = format!("{}{}{}{}", timestamp, self.api_key, RECV_WINDOW, query_string);
        let signature = self.sign(&payload);

        let url = if query_string.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}{}?{}", self.base_url, path, query_string)
        };

        let resp = self
            .http
            .get(&url)
            .header("X-BAPI-API-KEY", &self.api_key)
            .header("X-BAPI-TIMESTAMP", &timestamp)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-RECV-WINDOW", RECV_WINDOW)
            .send()
            .await
            .context("GET request to Bybit failed")?
            .json::<Value>()
            .await
            .context("Failed to parse Bybit GET response as JSON")?;

        check_ret_code(&resp)?;
        Ok(resp)
    }

    async fn post_signed(&self, path: &str, body: Value) -> Result<Value> {
        let body_string = serde_json::to_string(&body)?;
        let timestamp = Self::timestamp_ms();
        let payload = format!("{}{}{}{}", timestamp, self.api_key, RECV_WINDOW, body_string);
        let signature = self.sign(&payload);

        let url = format!("{}{}", self.base_url, path);

        let resp = self
            .http
            .post(&url)
            .header("X-BAPI-API-KEY", &self.api_key)
            .header("X-BAPI-TIMESTAMP", &timestamp)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-RECV-WINDOW", RECV_WINDOW)
            .header("Content-Type", "application/json")
            .body(body_string)
            .send()
            .await
            .context("POST request to Bybit failed")?
            .json::<Value>()
            .await
            .context("Failed to parse Bybit POST response as JSON")?;

        check_ret_code(&resp)?;
        Ok(resp)
    }

    /// Fetches every linear-category instrument (handles pagination).
    pub async fn get_all_linear_instruments(&self) -> Result<Vec<InstrumentInfo>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut params = vec![
                ("category".to_string(), "linear".to_string()),
                ("limit".to_string(), "1000".to_string()),
            ];
            if let Some(c) = &cursor {
                params.push(("cursor".to_string(), c.clone()));
            }
            let params_ref: Vec<(&str, String)> =
                params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

            let resp = self.get_signed("/v5/market/instruments-info", &params_ref).await?;
            let list = resp["result"]["list"].clone();
            let mut page: Vec<InstrumentInfo> = serde_json::from_value(list)?;
            all.append(&mut page);

            let next_cursor = resp["result"]["nextPageCursor"].as_str().unwrap_or("").to_string();
            if next_cursor.is_empty() {
                break;
            }
            cursor = Some(next_cursor);
        }

        Ok(all)
    }

    /// Fetches best bid/ask for every symbol in the linear category in one call.
    pub async fn get_all_tickers(&self) -> Result<HashMap<String, Ticker>> {
        let params = [("category", "linear".to_string())];
        let resp = self.get_signed("/v5/market/tickers", &params).await?;
        let list = resp["result"]["list"]
            .as_array()
            .ok_or_else(|| anyhow!("tickers response missing list"))?;

        let mut map = HashMap::new();
        for item in list {
            let symbol = item["symbol"].as_str().unwrap_or_default().to_string();
            let bid1 = item["bid1Price"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
            let ask1 = item["ask1Price"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
            if symbol.is_empty() || bid1 <= 0.0 || ask1 <= 0.0 {
                continue;
            }
            map.insert(
                symbol.clone(),
                Ticker {
                    symbol,
                    bid1_price: bid1,
                    ask1_price: ask1,
                },
            );
        }
        Ok(map)
    }

    pub async fn set_leverage(&self, symbol: &str, leverage: u32) -> Result<()> {
        let body = json!({
            "category": "linear",
            "symbol": symbol,
            "buyLeverage": leverage.to_string(),
            "sellLeverage": leverage.to_string(),
        });

        match self.post_signed("/v5/position/set-leverage", body).await {
            Ok(_) => Ok(()),
            Err(e) => {
                // 110043 = leverage not modified (already set) - safe to ignore
                if e.to_string().contains("110043") {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Places a market order. `reduce_only` should be true when closing a position.
    pub async fn place_market_order(
        &self,
        symbol: &str,
        side: &str, // "Buy" or "Sell"
        qty: &str,
        reduce_only: bool,
    ) -> Result<String> {
        let body = json!({
            "category": "linear",
            "symbol": symbol,
            "side": side,
            "orderType": "Market",
            "qty": qty,
            "reduceOnly": reduce_only,
            "timeInForce": "IOC",
        });

        let resp = self.post_signed("/v5/order/create", body).await?;
        let order_id = resp["result"]["orderId"].as_str().unwrap_or_default().to_string();

        info!(
            "ORDER PLACED: {} {} qty={} reduceOnly={} orderId={}",
            symbol, side, qty, reduce_only, order_id
        );

        Ok(order_id)
    }
}

fn check_ret_code(resp: &Value) -> Result<()> {
    let ret_code = resp["retCode"].as_i64().unwrap_or(-1);
    if ret_code != 0 {
        let ret_msg = resp["retMsg"].as_str().unwrap_or("unknown error");
        return Err(anyhow!("Bybit API error {}: {}", ret_code, ret_msg));
    }
    Ok(())
}

/// Number of decimal places implied by a step string like "0.001".
pub fn step_decimals(step_str: &str) -> usize {
    match step_str.split_once('.') {
        Some((_, frac)) => frac.len(),
        None => 0,
    }
}

/// Rounds `value` down to the nearest multiple of `step`, returned as an
/// exact multiple (computed via an integer step count to avoid float
/// drift), so the result can safely be bumped by one more step later.
pub fn floor_to_step(value: f64, step_str: &str) -> f64 {
    let step: f64 = step_str.parse().unwrap_or(1.0);
    if step <= 0.0 {
        return value.max(0.0);
    }
    let n_steps = (value / step).floor().max(0.0);
    n_steps * step
}

/// Formats a quantity that is already a multiple of `step_str` with the
/// right number of decimal places for the Bybit API.
pub fn format_qty(value: f64, step_str: &str) -> String {
    format!("{:.*}", step_decimals(step_str), value.max(0.0))
}