use futures_util::{SinkExt, StreamExt};
use mini_stock_monitor::{
    feed::{Endpoints, connect_sina, fetch_http, http_client},
    quote::{Source, normalize_symbol, parse_sina},
};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let symbols: Vec<_> = if args.is_empty() {
        vec!["sh600519".into(), "sz000001".into()]
    } else {
        args.iter()
            .map(|s| normalize_symbol(s))
            .collect::<Result<_, _>>()?
    };
    let endpoints = Endpoints::default();
    let client = http_client()?;
    for source in [Source::TencentHttp, Source::SinaHttp] {
        let parsed = fetch_http(&client, &endpoints, &symbols, source).await?;
        let missing: Vec<_> = symbols
            .iter()
            .filter(|symbol| !parsed.quotes.iter().any(|quote| &quote.symbol == *symbol))
            .collect();
        if !missing.is_empty() {
            return Err(format!("{} missing quotes: {missing:?}", source.label()).into());
        }
        println!("{}: {} quotes", source.label(), parsed.quotes.len());
        for quote in parsed.quotes {
            println!(
                "  {} {} {:?} {}",
                quote.symbol, quote.name, quote.price, quote.quote_time
            );
        }
    }
    let mut socket = connect_sina(&endpoints, &symbols).await?;
    println!("Sina WebSocket connected");
    let mut received = std::collections::BTreeSet::new();
    let mut pong = false;
    socket.send(Message::Text("".into())).await?;
    socket.send(Message::Ping(vec![0x4d, 0x53].into())).await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(frame) = socket.next().await {
            match frame.map_err(|error| error.to_string())? {
                Message::Text(text) => {
                    let parsed = parse_sina(&text, Source::SinaWebSocket);
                    for quote in parsed.quotes {
                        println!(
                            "  {} {} {:?} {}",
                            quote.symbol, quote.name, quote.price, quote.quote_time
                        );
                        received.insert(quote.symbol);
                    }
                }
                Message::Pong(_) => {
                    pong = true;
                    println!("WebSocket heartbeat: Pong received");
                }
                Message::Ping(_) => socket.flush().await.map_err(|error| error.to_string())?,
                Message::Close(_) => return Err("server closed socket".to_owned()),
                _ => {}
            }
            if received.len() == symbols.len() && pong {
                return Ok::<_, String>(());
            }
        }
        Err("socket ended before all quotes arrived".to_owned())
    })
    .await??;
    socket.close(None).await?;
    println!("PASS: both HTTP sources, all WebSocket subscriptions, and Ping/Pong");
    Ok(())
}
