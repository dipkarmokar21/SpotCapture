use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt}, path::{Path, PathBuf}, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};
use url::Url;
use crate::events::{emit, status};

pub const REDIRECT: &str = "http://127.0.0.1:5588/callback";

#[derive(Serialize, Deserialize)]
pub struct Tokens {
    pub client_id: String,
    pub access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub fn cache_dir(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match path {
        Some(path) => path,
        None => {
            let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
                .context("Cannot locate configuration directory; use --cache-dir")?;
            base.join("spotcapture")
        }
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(&path)?;
    ensure!(!fs::symlink_metadata(&path)?.file_type().is_symlink(), "Configuration directory must not be a symlink");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn save(path: &Path, tokens: &Tokens) -> Result<()> {
    save_secret(path, "tokens.json", tokens)
}

fn save_secret<T: Serialize>(path: &Path, filename: &str, value: &T) -> Result<()> {
    let temp = path.join(format!(".auth-{}-{}.tmp", std::process::id(), random_string()?));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::rename(&temp, path.join(filename))?;
        fs::File::open(path)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(temp); }
    result
}

fn random_string() -> Result<String> {
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn http_client() -> Result<Client> {
    Ok(Client::builder().connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30)).user_agent("SpotCapture/0.2")
        .redirect(reqwest::redirect::Policy::none()).build()?)
}

pub async fn login(client_id: String, cache: &Path) -> Result<Tokens> {
    ensure!(client_id.len() == 32 && client_id.bytes().all(|b| b.is_ascii_hexdigit()), "Enter your 32-character Spotify app Client ID (not its secret)");
    status(format!("Register {REDIRECT} in your Spotify app's Redirect URIs. Sign in using the link."));
    let token = authorize(&client_id, REDIRECT, "/callback", "streaming user-read-private user-read-email").await?;
    let tokens = Tokens { client_id, access_token:token.access_token,
        refresh_token:token.refresh_token.context("Spotify did not supply a refresh token; sign in again")?,
        expires_at:now().saturating_add(token.expires_in) };
    ensure!(!tokens.refresh_token.is_empty(), "Spotify supplied an empty refresh token; sign in again");
    save(cache, &tokens)?;
    Ok(tokens)
}

fn authorization_url(client_id: &str, redirect: &str, scopes: &str, state: &str, challenge: &str) -> Result<Url> {
    let mut url = Url::parse("https://accounts.spotify.com/authorize")?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id), ("response_type", "code"),
        ("redirect_uri", redirect), ("code_challenge_method", "S256"),
        ("code_challenge", challenge), ("state", state),
        ("scope", scopes),
    ]);
    Ok(url)
}

async fn authorize(client_id: &str, redirect: &str, callback_path: &str, scopes: &str) -> Result<TokenResponse> {
    let listener = TcpListener::bind("127.0.0.1:5588").await
        .context("Cannot open login callback on 127.0.0.1:5588; another login may be running")?;
    let verifier = random_string()?;
    let state = random_string()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let url = authorization_url(client_id, redirect, scopes, &state, &challenge)?;
    emit(serde_json::json!({"type":"login_url", "url":url.as_str(), "message":"Open this URL to sign in with Spotify"}));
    let code = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let mut request = Vec::new();
            let read_result = tokio::time::timeout(Duration::from_secs(3), async {
                let mut chunk = [0u8; 1024];
                loop {
                    let n = socket.read(&mut chunk).await?;
                    if n == 0 { break; }
                    request.extend_from_slice(&chunk[..n]);
                    ensure!(request.len() <= 8192, "Login request too large");
                    if request.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                }
                Ok::<_, anyhow::Error>(())
            }).await;
            if !matches!(read_result, Ok(Ok(()))) { continue; }
            let first = String::from_utf8_lossy(&request);
            let mut parts = first.lines().next().unwrap_or("").split_whitespace();
            if parts.next() != Some("GET") { continue; }
            let target = parts.next().unwrap_or("");
            let parsed = callback_code(target, &state, callback_path);
            let (http_status, body) = match &parsed {
                Ok(LoginCallback::Code(_)) => ("200 OK", "Spotify sign-in received. Return to SpotCapture."),
                Ok(LoginCallback::Declined) => ("200 OK", "Spotify sign-in was declined. Return to SpotCapture to try again."),
                Err(_) => ("400 Bad Request", "Invalid login callback. Use the original SpotCapture login link."),
            };
            let response = format!("HTTP/1.1 {http_status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes()).await;
            match parsed {
                Ok(LoginCallback::Code(code)) => return Ok::<_, anyhow::Error>(code),
                Ok(LoginCallback::Declined) => bail!("Spotify sign-in was declined; sign in again when ready"),
                Err(_) => {},
            }
        }
    }).await.context("Spotify sign-in timed out after 5 minutes")??;
    let response = http_client()?.post("https://accounts.spotify.com/api/token").form(&[
        ("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", redirect),
        ("client_id", client_id), ("code_verifier", verifier.as_str()),
    ]).send().await.context("Could not reach Spotify sign-in server")?;
    ensure!(response.status().is_success(), "Spotify token exchange failed (HTTP {}). Check Client ID and redirect URI", response.status());
    let token: TokenResponse = response.json().await
        .map_err(|_| anyhow::anyhow!("Invalid Spotify token response"))?;
    ensure!(!token.access_token.is_empty(), "Spotify supplied an empty access token; sign in again");
    Ok(token)
}

#[derive(Debug, PartialEq, Eq)]
enum LoginCallback {
    Code(String),
    Declined,
}

fn callback_code(target: &str, expected_state: &str, expected_path: &str) -> Result<LoginCallback> {
    ensure!(target.split_once('?').map(|(path, _)| path) == Some(expected_path), "Wrong callback path");
    let url = Url::parse(&format!("http://127.0.0.1:5588{target}"))?;
    let mut pairs = std::collections::HashMap::new();
    for (key, value) in url.query_pairs() {
        ensure!(pairs.insert(key, value).is_none(), "Repeated login callback parameter");
    }
    ensure!(pairs.get("state").map(|s| s.as_ref()) == Some(expected_state), "Login state mismatch");
    if pairs.contains_key("error") { return Ok(LoginCallback::Declined); }
    let code = pairs.get("code").context("Missing login code")?;
    ensure!(!code.is_empty(), "Empty login code");
    Ok(LoginCallback::Code(code.to_string()))
}

pub async fn load(cache: &Path, client_id: Option<&str>) -> Result<Tokens> {
    let bytes = fs::read(cache.join("tokens.json")).context("Web API not connected. Use spotcapture login --client-id YOUR_ID for optional artwork and metadata")?;
    let mut tokens: Tokens = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Saved login is invalid; sign in again"))?;
    ensure!(!tokens.access_token.is_empty() && !tokens.refresh_token.is_empty(), "Saved login is incomplete; sign in again");
    if let Some(id) = client_id.filter(|id| !id.is_empty()) {
        ensure!(id == tokens.client_id, "Client ID changed; sign in again with this Client ID");
    }
    if tokens.expires_at > now() + 60 { return Ok(tokens); }
    let response = http_client()?.post("https://accounts.spotify.com/api/token").form(&[
        ("grant_type", "refresh_token"), ("refresh_token", tokens.refresh_token.as_str()),
        ("client_id", tokens.client_id.as_str()),
    ]).send().await.context("Could not refresh Spotify login")?;
    ensure!(response.status().is_success(), "Spotify login refresh failed (HTTP {}); sign in again", response.status());
    let token: TokenResponse = response.json().await
        .map_err(|_| anyhow::anyhow!("Invalid Spotify token response; sign in again"))?;
    ensure!(!token.access_token.is_empty(), "Spotify supplied an empty access token; sign in again");
    tokens.access_token = token.access_token;
    if let Some(refresh) = token.refresh_token.filter(|value| !value.is_empty()) { tokens.refresh_token = refresh; }
    tokens.expires_at = now().saturating_add(token.expires_in);
    save(cache, &tokens)?;
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_requires_matching_state_and_path() {
        assert_eq!(callback_code("/callback?code=hello&state=good", "good", "/callback").unwrap(), LoginCallback::Code("hello".into()));
        assert!(callback_code("/callback?code=hello&state=bad", "good", "/callback").is_err());
        assert!(callback_code("/elsewhere?code=hello&state=good", "good", "/callback").is_err());
    }
    #[test]
    fn authenticated_denial_finishes_login_but_foreign_denial_does_not() {
        assert_eq!(callback_code("/callback?error=access_denied&state=good", "good", "/callback").unwrap(), LoginCallback::Declined);
        assert!(callback_code("/callback?error=access_denied&state=bad", "good", "/callback").is_err());
        assert!(callback_code("/callback?error=access_denied", "good", "/callback").is_err());
    }
    #[test]
    fn ambiguous_or_empty_callback_parameters_are_rejected() {
        assert!(callback_code("/callback?code=hello&state=bad&state=good", "good", "/callback").is_err());
        assert!(callback_code("/callback?code=first&code=second&state=good", "good", "/callback").is_err());
        assert!(callback_code("/callback?code=&state=good", "good", "/callback").is_err());
    }
}
