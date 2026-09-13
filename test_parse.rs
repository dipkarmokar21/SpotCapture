use std::collections::HashSet;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M";
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .build()?;
    let body = client.get(url).send().await?.text().await?;
    
    if let Some(start) = body.find(r#"<script id="initial-state" type="text/plain">"#) {
        let json_start = start + 45;
        if let Some(end) = body[json_start..].find("</script>") {
            let json_str = &body[json_start..json_start + end];
            let decoded = base64::decode(json_str);
            if let Ok(bytes) = decoded {
                let s = String::from_utf8_lossy(&bytes);
                println!("Decoded starts with: {:.100}", s);
            } else {
                let v: serde_json::Value = serde_json::from_str(json_str)?;
                println!("Got JSON object");
            }
        }
    } else {
        println!("No initial-state found");
    }
    Ok(())
}
