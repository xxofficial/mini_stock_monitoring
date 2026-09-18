use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use chrono::{DateTime, Datelike, FixedOffset, Timelike, Utc, Weekday};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    sync::{oneshot, watch},
    time::{self, Instant, MissedTickBehavior},
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

use crate::{
    config::{FeedMode, Settings},
    quote::{ParsedQuotes, Quote, Source, parse_sina, parse_tencent},
};

pub type Wake = Arc<dyn Fn() + Send + Sync>;
pub type SharedSnapshot = Arc<Mutex<Snapshot>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Connecting,
    Streaming,
    Fallback,
    Offline,
    Paused,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub quotes: BTreeMap<String, Quote>,
    pub errors: BTreeMap<String, String>,
    pub phase: Phase,
    pub detail: String,
    pub last_received: Option<DateTime<Utc>>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            quotes: BTreeMap::new(),
            errors: BTreeMap::new(),
            phase: Phase::Connecting,
            detail: "正在连接新浪行情…".into(),
            last_received: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FeedConfig {
    pub symbols: Vec<String>,
    pub poll_seconds: u64,
    pub mode: FeedMode,
    pub paused: bool,
}

impl From<&Settings> for FeedConfig {
    fn from(settings: &Settings) -> Self {
        Self {
            symbols: settings.symbols.clone(),
            poll_seconds: settings.poll_seconds,
            mode: settings.feed_mode,
            paused: false,
        }
    }
}

#[derive(Clone)]
pub struct Endpoints {
    pub sina_ws: String,
    pub tencent_http: String,
    pub sina_http: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            sina_ws: "wss://hq.sinajs.cn/wskt?list=".into(),
            tencent_http: "https://qt.gtimg.cn/q=".into(),
            sina_http: "https://hq.sinajs.cn/list=".into(),
        }
    }
}

pub struct Feed {
    config: watch::Sender<FeedConfig>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
    pub snapshot: SharedSnapshot,
}

impl Feed {
    pub fn spawn(config: FeedConfig, wake: Wake) -> std::io::Result<Self> {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let shared = snapshot.clone();
        let (config_tx, config_rx) = watch::channel(config);
        let (stop_tx, stop_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("quote-feed".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        set_phase(
                            &shared,
                            &wake,
                            Phase::Offline,
                            format!("后台服务启动失败：{error}"),
                        );
                        return;
                    }
                };
                runtime.block_on(run_worker(
                    config_rx,
                    stop_rx,
                    shared,
                    wake,
                    Endpoints::default(),
                ));
            })?;
        Ok(Self {
            config: config_tx,
            stop: Some(stop_tx),
            worker: Some(worker),
            snapshot,
        })
    }

    pub fn configure(&self, config: FeedConfig) {
        if let Ok(mut state) = self.snapshot.lock() {
            state.quotes.retain(|key, _| config.symbols.contains(key));
            state.errors.clear();
        }
        self.config.send_replace(config);
    }

    pub fn latest(&self) -> Snapshot {
        self.snapshot
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn run_worker(
    mut config: watch::Receiver<FeedConfig>,
    mut stop: oneshot::Receiver<()>,
    state: SharedSnapshot,
    wake: Wake,
    endpoints: Endpoints,
) {
    let client = match http_client() {
        Ok(client) => client,
        Err(error) => {
            set_phase(&state, &wake, Phase::Offline, error);
            return;
        }
    };
    loop {
        let current = config.borrow_and_update().clone();
        tokio::select! {
            _ = &mut stop => return,
            changed = config.changed() => { if changed.is_err() { return; } },
            _ = run_config(&current, &client, &endpoints, &state, &wake) => return,
        }
    }
}

async fn run_config(
    config: &FeedConfig,
    client: &reqwest::Client,
    endpoints: &Endpoints,
    state: &SharedSnapshot,
    wake: &Wake,
) {
    if config.paused || config.symbols.is_empty() {
        let (phase, message) = if config.paused {
            (Phase::Paused, "行情更新已暂停")
        } else {
            (Phase::Idle, "添加一只股票，开始关注行情")
        };
        set_phase(state, wake, phase, message.into());
        std::future::pending::<()>().await;
        return;
    }
    if config.mode == FeedMode::Tencent {
        loop {
            poll_http(config, client, endpoints, state, wake, None).await;
            time::sleep(poll_interval(config, Utc::now())).await;
        }
    }

    let mut failures: u32 = 0;
    let mut next_http = Instant::now();
    loop {
        if failures == 0 {
            set_phase(state, wake, Phase::Connecting, "正在连接新浪行情…".into());
        }
        let connected_at = Instant::now();
        let failure = match stream_sina(config, endpoints, state, wake).await {
            Ok(()) => "新浪连接已结束".to_owned(),
            Err(error) => error,
        };
        if connected_at.elapsed() > Duration::from_secs(60) {
            failures = 0;
        }
        failures = failures.saturating_add(1);
        // Back off reconnects separately from HTTP polling, so a flapping socket
        // cannot cause HTTP requests to run faster than the configured interval.
        let delay = Duration::from_secs((1u64 << failures.min(5)).min(30));
        let retry_at = Instant::now() + delay;
        set_phase(
            state,
            wake,
            Phase::Fallback,
            format!("{failure}；正在使用备用行情"),
        );
        while Instant::now() < retry_at {
            if Instant::now() >= next_http {
                poll_http(config, client, endpoints, state, wake, Some(&failure)).await;
                next_http = Instant::now() + poll_interval(config, Utc::now());
            }
            time::sleep_until(next_http.min(retry_at)).await;
        }
    }
}

pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 MiniStockMonitor/0.1")
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(7))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("无法创建行情连接：{error}"))
}

fn sina_subscription(symbols: &[String]) -> String {
    symbols
        .iter()
        .map(|symbol| {
            if symbol.starts_with("hk") {
                format!("rt_{symbol}")
            } else {
                symbol.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub async fn fetch_http(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    symbols: &[String],
    source: Source,
) -> Result<ParsedQuotes, String> {
    let (base, referer, encoding) = match source {
        Source::TencentHttp => (
            &endpoints.tencent_http,
            "https://gu.qq.com/",
            encoding_rs::GBK,
        ),
        _ => (
            &endpoints.sina_http,
            "https://finance.sina.com.cn/",
            encoding_rs::GB18030,
        ),
    };
    let subscription = if source == Source::TencentHttp {
        symbols.join(",")
    } else {
        sina_subscription(symbols)
    };
    let response = client
        .get(format!("{base}{subscription}"))
        .header("Referer", referer)
        .send()
        .await
        .map_err(|error| format!("行情请求失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("行情服务器暂不可用：{error}"))?;
    if response
        .content_length()
        .is_some_and(|size| size > 1_048_576)
    {
        return Err("行情响应过大".into());
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("行情读取失败：{error}"))?;
    if body.len() > 1_048_576 {
        return Err("行情响应过大".into());
    }
    let (text, _, malformed) = encoding.decode(&body);
    if malformed {
        return Err("行情文字编码异常".into());
    }
    let parsed = if source == Source::TencentHttp {
        parse_tencent(&text)
    } else {
        parse_sina(&text, Source::SinaHttp)
    };
    if parsed.quotes.is_empty() {
        return Err("未收到有效行情，请检查股票代码或稍后重试".into());
    }
    Ok(parsed)
}

async fn poll_http(
    config: &FeedConfig,
    client: &reqwest::Client,
    endpoints: &Endpoints,
    state: &SharedSnapshot,
    wake: &Wake,
    ws_error: Option<&str>,
) {
    let mut result = fetch_http(client, endpoints, &config.symbols, Source::TencentHttp).await;
    if result.is_err() && config.mode == FeedMode::Auto {
        result = fetch_http(client, endpoints, &config.symbols, Source::SinaHttp).await;
    }
    match result {
        Ok(parsed) => {
            let source = parsed
                .quotes
                .first()
                .map(|quote| quote.source.label())
                .unwrap_or("备用行情");
            apply_quotes(state, &config.symbols, parsed, wake);
            let detail = match ws_error {
                Some(error) => format!("{source}定时更新；新浪推送重连中。{error}"),
                None => format!("{source}定时更新"),
            };
            set_phase(state, wake, Phase::Fallback, detail);
        }
        Err(error) => set_phase(
            state,
            wake,
            Phase::Offline,
            format!("{error}；保留最后一次行情"),
        ),
    }
}

pub async fn connect_sina(
    endpoints: &Endpoints,
    symbols: &[String],
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let mut request = format!("{}{}", endpoints.sina_ws, sina_subscription(symbols))
        .into_client_request()
        .map_err(|error| error.to_string())?;
    request.headers_mut().insert(
        "Origin",
        "https://finance.sina.com.cn"
            .parse()
            .expect("constant header"),
    );
    request.headers_mut().insert(
        "User-Agent",
        "Mozilla/5.0 MiniStockMonitor/0.1"
            .parse()
            .expect("constant header"),
    );
    let (socket, _) = time::timeout(
        Duration::from_secs(8),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| "新浪连接超时".to_string())?
    .map_err(|error| format!("新浪连接失败：{error}"))?;
    Ok(socket)
}

async fn stream_sina(
    config: &FeedConfig,
    endpoints: &Endpoints,
    state: &SharedSnapshot,
    wake: &Wake,
) -> Result<(), String> {
    let mut socket = connect_sina(endpoints, &config.symbols).await?;
    let mut heartbeat = time::interval_at(
        Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    );
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut watchdog = time::interval(Duration::from_secs(5));
    watchdog.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut received_quote = false;
    let started = Instant::now();
    let mut last_frame = Instant::now();
    loop {
        tokio::select! {
            frame = socket.next() => {
                let frame = frame.ok_or_else(|| "新浪推送连接断开".to_string())?
                    .map_err(|error| format!("新浪推送中断：{error}"))?;
                last_frame = Instant::now();
                match frame {
                    Message::Text(text) => {
                        let parsed = parse_sina(&text, Source::SinaWebSocket);
                        if let Some(error) = parsed.server_error.as_ref() { return Err(error.clone()); }
                        if parsed.quotes.iter().any(|quote| config.symbols.contains(&quote.symbol)) {
                            received_quote = true;
                            set_phase(state, wake, Phase::Streaming, "新浪推送已连接".into());
                        }
                        apply_quotes(state, &config.symbols, parsed, wake);
                    }
                    Message::Ping(_) => { socket.flush().await.map_err(|error| error.to_string())?; }
                    Message::Pong(_) => {}
                    Message::Close(_) => return Err("新浪服务器关闭了连接".into()),
                    Message::Binary(_) => return Err("新浪行情格式发生变化".into()),
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                // Sina's own client sends an empty text heartbeat. Protocol Ping
                // additionally lets us detect a half-open connection while prices are idle.
                socket.send(Message::Text("".into())).await.map_err(|error| error.to_string())?;
                socket.send(Message::Ping(vec![0x4d, 0x53].into())).await.map_err(|error| error.to_string())?;
            }
            _ = watchdog.tick() => {
                if !received_quote && started.elapsed() > Duration::from_secs(12) {
                    return Err("新浪连接未返回有效行情".into());
                }
                if last_frame.elapsed() > Duration::from_secs(100) {
                    return Err("新浪连接长时间无响应".into());
                }
            }
        }
    }
}

fn set_phase(state: &SharedSnapshot, wake: &Wake, phase: Phase, detail: String) {
    let changed = if let Ok(mut state) = state.lock() {
        let changed = state.phase != phase || state.detail != detail;
        state.phase = phase;
        state.detail = detail;
        changed
    } else {
        false
    };
    if changed {
        wake();
    }
}

fn apply_quotes(state: &SharedSnapshot, subscribed: &[String], parsed: ParsedQuotes, wake: &Wake) {
    let mut changed = false;
    if let Ok(mut state) = state.lock() {
        for symbol in parsed.missing {
            if subscribed.contains(&symbol) {
                state.errors.insert(symbol, "未找到行情，请检查代码".into());
                changed = true;
            }
        }
        for quote in parsed.quotes {
            if !subscribed.contains(&quote.symbol) {
                continue;
            }
            state.last_received = Some(quote.received_at);
            state.errors.remove(&quote.symbol);
            // A recovered source must not overwrite a newer quote with an older snapshot.
            if state
                .quotes
                .get(&quote.symbol)
                .is_none_or(|old| quote.quote_time >= old.quote_time)
            {
                state.quotes.insert(quote.symbol.clone(), quote);
            }
            changed = true;
        }
    }
    if changed {
        wake();
    }
}

/// Conservative session window, including auctions. Holidays are intentionally
/// not inferred: on a weekday holiday we keep probing, rather than miss a session.
fn poll_interval(config: &FeedConfig, now: DateTime<Utc>) -> Duration {
    let china = now.with_timezone(&FixedOffset::east_opt(8 * 3600).expect("UTC+8"));
    let minute = china.hour() * 60 + china.minute();
    let weekday = !matches!(china.weekday(), Weekday::Sat | Weekday::Sun);
    let mainland_hours = (550..=695).contains(&minute) || (775..=910).contains(&minute);
    let hk_hours = config.symbols.iter().any(|symbol| symbol.starts_with("hk"))
        && ((535..=735).contains(&minute) || (775..=975).contains(&minute));
    let market_hours = weekday && (mainland_hours || hk_hours);
    Duration::from_secs(if market_hours {
        config.poll_seconds.clamp(2, 60)
    } else {
        60
    })
}

pub fn missing_symbols(snapshot: &Snapshot, symbols: &[String]) -> BTreeSet<String> {
    symbols
        .iter()
        .filter(|symbol| !snapshot.quotes.contains_key(*symbol))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const QUOTE: &str = "sh600519=贵州茅台,1262.990,1266.980,1257.120,1265.880,1256.100,1257.120,1257.130,2489087,3135849108.000,831,1257.120,200,1257.110,100,1257.080,200,1257.060,200,1257.050,100,1257.130,200,1257.240,100,1257.280,1600,1258.000,100,1258.280,2026-09-18,15:34:59,00";
    const HK_QUOTE: &str = "rt_hk00700=TENCENT,腾讯控股,428.000,426.000,430.400,419.000,419.000,-7.000,-1.643,418.800,419.000,12180786280.956,28796138,15.229,0.000,675.134,411.000,2026/09/18,16:08:32";

    #[test]
    fn hk_polling_covers_its_morning_and_later_close_without_changing_mainland_hours() {
        let mainland = FeedConfig::from(&Settings::default());
        let mut mixed = mainland.clone();
        mixed.symbols.push("hk00700".into());
        for (stamp, mainland_seconds, hk_seconds) in [
            ("2026-09-18T09:05:00+08:00", 60, 3),
            ("2026-09-18T10:30:00+08:00", 3, 3),
            ("2026-09-18T11:50:00+08:00", 60, 3),
            ("2026-09-18T12:09:00+08:00", 60, 3),
            ("2026-09-18T12:30:00+08:00", 60, 60),
            ("2026-09-18T15:30:00+08:00", 60, 3),
            ("2026-09-18T16:09:00+08:00", 60, 3),
            ("2026-09-18T16:16:00+08:00", 60, 60),
            ("2026-09-19T10:30:00+08:00", 60, 60),
        ] {
            let now = DateTime::parse_from_rfc3339(stamp)
                .unwrap()
                .with_timezone(&Utc);
            assert_eq!(
                poll_interval(&mainland, now).as_secs(),
                mainland_seconds,
                "{stamp}"
            );
            assert_eq!(poll_interval(&mixed, now).as_secs(), hk_seconds, "{stamp}");
        }
    }

    #[tokio::test]
    async fn sina_http_requests_hk_channel_and_returns_canonical_symbols() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoints = Endpoints {
            sina_http: format!("http://{}/?list=", listener.local_addr().unwrap()),
            ..Endpoints::default()
        };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0 && request.len() + count < 8192);
                request.extend_from_slice(&buffer[..count]);
            }
            assert!(
                String::from_utf8_lossy(&request).starts_with("GET /?list=sh600519,rt_hk00700 ")
            );
            let payload = format!("{QUOTE}\n{HK_QUOTE}");
            let (body, _, _) = encoding_rs::GB18030.encode(&payload);
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&body).await.unwrap();
        });
        let symbols = vec!["sh600519".into(), "hk00700".into()];
        let parsed = fetch_http(
            &http_client().unwrap(),
            &endpoints,
            &symbols,
            Source::SinaHttp,
        )
        .await
        .unwrap();
        assert_eq!(
            parsed
                .quotes
                .iter()
                .map(|quote| quote.symbol.clone())
                .collect::<Vec<_>>(),
            symbols
        );
        server.await.unwrap();
    }

    #[test]
    fn never_replaces_newer_prices_with_old_snapshots() {
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let wake: Wake = Arc::new(|| {});
        let symbols = vec!["sh600519".into()];
        apply_quotes(
            &state,
            &symbols,
            parse_sina(QUOTE, Source::SinaWebSocket),
            &wake,
        );
        apply_quotes(
            &state,
            &symbols,
            parse_sina(
                &QUOTE
                    .replace("15:34:59", "15:30:00")
                    .replace("1257.120", "1.000"),
                Source::SinaHttp,
            ),
            &wake,
        );
        assert_eq!(
            state.lock().unwrap().quotes["sh600519"].price,
            Some(1257.12)
        );
    }

    #[tokio::test]
    async fn broken_websocket_falls_back_and_worker_stops_promptly() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut connection, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut request = [0; 4096];
                    let _ = connection.read(&mut request).await;
                    let text = format!("var hq_str_{};", QUOTE.replacen('=', "=\"", 1) + "\"");
                    let (body, _, _) = encoding_rs::GB18030.encode(&text);
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = connection.write_all(header.as_bytes()).await;
                    let _ = connection.write_all(&body).await;
                });
            }
        });
        let endpoints = Endpoints {
            sina_ws: format!("ws://{address}/ws?list="),
            tencent_http: format!("http://{address}/?q="),
            sina_http: format!("http://{address}/?list="),
        };
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let (config_tx, config_rx) = watch::channel(FeedConfig::from(&Settings {
            symbols: vec!["sh600519".into()],
            ..Settings::default()
        }));
        let (stop_tx, stop_rx) = oneshot::channel();
        let worker = tokio::spawn(run_worker(
            config_rx,
            stop_rx,
            state.clone(),
            Arc::new(|| {}),
            endpoints,
        ));
        time::timeout(Duration::from_secs(4), async {
            loop {
                if !state.lock().unwrap().quotes.is_empty() {
                    break;
                }
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            state.lock().unwrap().quotes["sh600519"].source,
            Source::SinaHttp
        );
        config_tx.send_modify(|config| config.paused = true);
        time::timeout(Duration::from_secs(1), async {
            while state.lock().unwrap().phase != Phase::Paused {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop_tx.send(()).unwrap();
        time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite requires an HTTP response as its handshake callback error"
    )]
    async fn streams_multiple_quotes_and_cancels_old_subscription_on_change() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                connection,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(
                        request.uri().query(),
                        Some("list=sh600519,sz000001,rt_hk00700")
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            socket
                .send(Message::Text(
                    format!(
                        "{QUOTE}\n{}\n{HK_QUOTE}",
                        QUOTE.replace("sh600519", "sz000001")
                    )
                    .into(),
                ))
                .await
                .unwrap();
            while socket.next().await.is_some() {}
        });
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let settings = Settings {
            symbols: vec!["sh600519".into(), "sz000001".into(), "hk00700".into()],
            ..Settings::default()
        };
        let (tx, rx) = watch::channel(FeedConfig::from(&settings));
        let (stop_tx, stop_rx) = oneshot::channel();
        let endpoints = Endpoints {
            sina_ws: format!("ws://{address}/?list="),
            ..Endpoints::default()
        };
        let worker = tokio::spawn(run_worker(
            rx,
            stop_rx,
            state.clone(),
            Arc::new(|| {}),
            endpoints,
        ));
        time::timeout(Duration::from_secs(2), async {
            while state.lock().unwrap().quotes.len() != 3 {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.lock().unwrap().phase, Phase::Streaming);
        assert_eq!(state.lock().unwrap().quotes["hk00700"].price, Some(419.0));
        tx.send_modify(|config| config.symbols.clear());
        time::timeout(Duration::from_secs(1), async {
            while state.lock().unwrap().phase != Phase::Idle {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop_tx.send(()).unwrap();
        worker.await.unwrap();
        server.abort();
    }
}
