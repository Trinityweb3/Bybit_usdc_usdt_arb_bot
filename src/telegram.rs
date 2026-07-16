use anyhow::{anyhow, Result};
use serde_json::Value;

/// Sends a plain text message via the Telegram Bot API
/// (`POST https://api.telegram.org/bot<token>/sendMessage`).
pub async fn send_message(
    http: &reqwest::Client,
    bot_token: &str,
    chat_id: &str,
    text: &str,
) -> Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);

    let resp = http
        .post(&url)
        .form(&[("chat_id", chat_id), ("text", text)])
        .send()
        .await?
        .json::<Value>()
        .await?;

    let ok = resp["ok"].as_bool().unwrap_or(false);
    if !ok {
        let desc = resp["description"].as_str().unwrap_or("unknown error");
        return Err(anyhow!("Telegram sendMessage failed: {}", desc));
    }
    Ok(())
}

pub async fn verify_chat(http: &reqwest::Client, bot_token: &str, chat_id: &str) -> Result<()> {
    let url = format!("https://api.telegram.org/bot{}/getChat", bot_token);

    let resp = http
        .get(&url)
        .query(&[("chat_id", chat_id)])
        .send()
        .await?
        .json::<Value>()
        .await?;

    let ok = resp["ok"].as_bool().unwrap_or(false);
    if !ok {
        let desc = resp["description"].as_str().unwrap_or("unknown error");
        return Err(anyhow!("Telegram getChat failed: {}", desc));
    }
    Ok(())
}