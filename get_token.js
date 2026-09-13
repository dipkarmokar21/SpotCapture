fetch("https://open.spotify.com/get_access_token?reason=transport&productType=web_player", {
    headers: {
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
        "Accept": "application/json"
    }
}).then(r => r.text()).then(console.log).catch(console.error);
