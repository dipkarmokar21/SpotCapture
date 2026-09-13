use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let token = "BQBvwJjC1oU5w5kZ4WfE5-BwA_Q3i4M... (wait, I need a client id/secret)";
    Ok(())
}
