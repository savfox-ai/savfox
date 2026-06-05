/// Send a message via the Zalo OA Customer Service API.
pub async fn send_message(
    client: &reqwest::Client,
    access_token: &str,
    user_id: &str,
    text: &str,
) -> anyhow::Result<()> {
    let url = "https://openapi.zalo.me/v3.0/oa/message/cs";
    let body = serde_json::json!({
        "recipient": {
            "user_id": user_id,
        },
        "message": {
            "text": text,
        },
    });

    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("access_token", access_token)
        .json(&body)
        .send()
        .await?;

    crate::http::warn_on_error(response, "Zalo OA API error").await;
    Ok(())
}
