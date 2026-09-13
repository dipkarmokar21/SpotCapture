#[tokio::main]
async fn main() {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .build().unwrap();
    let res = client.get("https://open.spotify.com/get_access_token?reason=transport&productType=web_player")
        .header("Accept", "application/json")
        .send().await.unwrap();
    println!("Status: {}", res.status());
    println!("Body: {}", res.text().await.unwrap());
}
