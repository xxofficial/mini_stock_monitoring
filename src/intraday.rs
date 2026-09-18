use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike, Utc, Weekday};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::{
    feed::{Wake, http_client},
    quote::normalize_symbol,
};

const ENDPOINT: &str = "https://web.ifzq.gtimg.cn/appstock/app/minute/query";
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct MinutePoint {
    /// Local exchange time (UTC+8), in minutes since midnight.
    pub minute: u16,
    pub price: f64,
}

impl MinutePoint {
    pub fn time_label(&self) -> String {
        format!("{:02}:{:02}", self.minute / 60, self.minute % 60)
    }
}

#[derive(Clone, Debug)]
pub struct IntradaySeries {
    pub symbol: String,
    pub name: String,
    pub date: NaiveDate,
    pub previous_close: Option<f64>,
    pub points: Vec<MinutePoint>,
}

impl IntradaySeries {
    pub fn price_range(&self) -> (f64, f64) {
        let low = self.points.iter().map(|p| p.price).reduce(f64::min);
        let high = self.points.iter().map(|p| p.price).reduce(f64::max);
        let center = self
            .previous_close
            .unwrap_or_else(|| (low.unwrap_or(1.0) + high.unwrap_or(1.0)) / 2.0);
        // A symmetric scale makes the previous-close line a true zero baseline,
        // and remains drawable for a flat session or a single minute.
        let radius = (high.unwrap_or(center) - center)
            .abs()
            .max((low.unwrap_or(center) - center).abs())
            .max(center * 0.002)
            * 1.12;
        (center - radius, center + radius)
    }
}

/// Compress lunch, but never extrapolate prices into pre-open or future minutes.
/// HK's closing auction is retained through 16:10; mainland ends at 15:00.
pub fn session_position(symbol: &str, minute: u16) -> Option<f32> {
    let hk = symbol.starts_with("hk");
    let morning_end = if hk { 720 } else { 690 };
    let close = if hk { 970 } else { 900 };
    let morning = morning_end - 570;
    let offset = if (570..=morning_end).contains(&minute) {
        minute - 570
    } else if (780..=close).contains(&minute) {
        morning + minute - 780
    } else {
        return None;
    };
    Some(f32::from(offset) / f32::from(morning + close - 780))
}

fn positive(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number > 0.0 && *number < 1e12)
}

fn minute(value: &str) -> Option<u16> {
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hour: u16 = value[..2].parse().ok()?;
    let minute: u16 = value[2..].parse().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

pub fn parse_intraday(payload: &[u8], symbol: &str) -> Result<IntradaySeries, String> {
    let symbol = normalize_symbol(symbol)?;
    let root: Value = serde_json::from_slice(payload).map_err(|_| "分时数据格式异常".to_owned())?;
    if root.get("code").and_then(Value::as_i64) != Some(0) {
        return Err("分时服务暂不可用，请稍后重试".into());
    }
    let item = &root["data"][&symbol];
    let data = &item["data"];
    let date = data["date"]
        .as_str()
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y%m%d").ok())
        .ok_or_else(|| "暂无分时数据，请稍后重试".to_owned())?;
    let rows = data["data"]
        .as_array()
        .ok_or_else(|| "分时数据格式异常".to_owned())?;
    let mut points = BTreeMap::new();
    for row in rows {
        let Some(row) = row.as_str() else { continue };
        let mut fields = row.split_whitespace();
        let Some(minute) = fields.next().and_then(minute) else {
            continue;
        };
        let Some(price) = fields.next().and_then(positive) else {
            continue;
        };
        if session_position(&symbol, minute).is_some() {
            points.insert(minute, MinutePoint { minute, price });
        }
    }
    if !rows.is_empty() && points.is_empty() {
        return Err("未收到有效分时价格，请稍后重试".into());
    }
    let quote = &item["qt"][&symbol];
    // The snapshot can roll to a new day before the minute history does. Never
    // use that new day's previous close for yesterday's chart.
    let quote_date = quote.get(30).and_then(Value::as_str).and_then(|stamp| {
        let digits: String = stamp.chars().filter(char::is_ascii_digit).take(8).collect();
        NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
    });
    let previous_close = (quote_date == Some(date))
        .then(|| quote.get(4).and_then(Value::as_str).and_then(positive))
        .flatten();
    Ok(IntradaySeries {
        name: quote
            .get(1)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&symbol)
            .into(),
        symbol,
        date,
        previous_close,
        points: points.into_values().collect(),
    })
}

pub async fn fetch_intraday(
    client: &reqwest::Client,
    symbol: &str,
) -> Result<IntradaySeries, String> {
    fetch_from(client, ENDPOINT, symbol).await
}

async fn fetch_from(
    client: &reqwest::Client,
    endpoint: &str,
    symbol: &str,
) -> Result<IntradaySeries, String> {
    let symbol = normalize_symbol(symbol)?;
    let mut response = client
        .get(format!("{endpoint}?code={symbol}"))
        .header("Referer", "https://gu.qq.com/")
        .send()
        .await
        .map_err(|_| "分时连接失败，请检查网络".to_owned())?
        .error_for_status()
        .map_err(|_| "分时服务暂不可用".to_owned())?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err("分时响应过大".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "分时数据读取失败".to_owned())?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("分时响应过大".into());
        }
        body.extend_from_slice(&chunk);
    }
    parse_intraday(&body, &symbol)
}

#[derive(Clone, Debug, Default)]
pub struct IntradaySnapshot {
    generation: u64,
    pub symbol: Option<String>,
    pub loading: bool,
    pub series: Option<Arc<IntradaySeries>>,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
struct Request {
    generation: u64,
    symbol: Option<String>,
    paused: bool,
}

type Shared = Arc<Mutex<IntradaySnapshot>>;

/// Only the visible chart is polled. Switching stocks, pausing, or closing the
/// chart cancels in-flight HTTP and prevents stale results from being published.
pub struct IntradayFeed {
    requests: watch::Sender<Request>,
    snapshot: Shared,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl IntradayFeed {
    pub fn spawn(wake: Wake) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let snapshot = Arc::new(Mutex::new(IntradaySnapshot::default()));
        let shared = snapshot.clone();
        let (requests, receiver) = watch::channel(Request::default());
        let (stop, stop_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("intraday-feed".into())
            .spawn(move || {
                runtime.block_on(run_worker(receiver, stop_rx, shared, wake, ENDPOINT.into()));
            })?;
        Ok(Self {
            requests,
            snapshot,
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    pub fn request(&self, symbol: Option<&str>, paused: bool) {
        if let Ok(mut state) = self.snapshot.lock() {
            let symbol = symbol.and_then(|symbol| normalize_symbol(symbol).ok());
            if state.symbol != symbol {
                state.series = None;
            }
            state.generation = state.generation.wrapping_add(1);
            state.symbol = symbol.clone();
            state.loading = symbol.is_some() && !paused;
            state.error = None;
            self.requests.send_replace(Request {
                generation: state.generation,
                symbol,
                paused,
            });
        }
    }

    pub fn latest(&self) -> IntradaySnapshot {
        self.snapshot
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

impl Drop for IntradayFeed {
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
    mut requests: watch::Receiver<Request>,
    mut stop: oneshot::Receiver<()>,
    state: Shared,
    wake: Wake,
    endpoint: String,
) {
    let client = http_client();
    loop {
        let current = requests.borrow_and_update().clone();
        tokio::select! {
            biased;
            _ = &mut stop => return,
            changed = requests.changed() => { if changed.is_err() { return; } },
            _ = async {
                if let Some(symbol) = &current.symbol && !current.paused {
                    loop {
                        let result = match &client {
                            Ok(client) => fetch_from(client, &endpoint, symbol).await,
                            Err(_) => Err("分时网络服务启动失败".into()),
                        };
                        complete(&state, &current, result, &wake);
                        tokio::time::sleep(refresh_interval(symbol, Utc::now())).await;
                    }
                }
                std::future::pending::<()>().await;
            } => {},
        }
    }
}

fn complete(
    state: &Shared,
    request: &Request,
    result: Result<IntradaySeries, String>,
    wake: &Wake,
) {
    if let Ok(mut state) = state.lock() {
        if state.generation != request.generation || state.symbol != request.symbol {
            return;
        }
        state.loading = false;
        match result {
            Ok(series) => {
                let regressed = state.series.as_ref().is_some_and(|old| {
                    series.date < old.date
                        || (series.date == old.date
                            && series.points.last().map(|p| p.minute)
                                < old.points.last().map(|p| p.minute))
                });
                if regressed {
                    state.error = Some("分时数据暂未更新，保留上次走势".into());
                } else {
                    state.series = Some(Arc::new(series));
                    state.error = None;
                }
            }
            Err(error) => state.error = Some(error),
        }
    }
    wake();
}

fn refresh_interval(symbol: &str, now: DateTime<Utc>) -> Duration {
    let now = now.with_timezone(&FixedOffset::east_opt(28800).expect("UTC+8"));
    let minute = (now.hour() * 60 + now.minute()) as u16;
    // Keep probing shortly after each close so the final minute/auction arrives.
    let morning_end = if symbol.starts_with("hk") { 725 } else { 695 };
    let close = if symbol.starts_with("hk") { 975 } else { 905 };
    let active = !matches!(now.weekday(), Weekday::Sat | Weekday::Sun)
        && ((565..=morning_end).contains(&minute) || (775..=close).contains(&minute));
    Duration::from_secs(if active { 15 } else { 60 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn payload(symbol: &str, date: &str, rows: &[&str]) -> Vec<u8> {
        let mut quote = vec!["".to_owned(); 31];
        quote[1] = "测试股票".into();
        quote[4] = "10.00".into();
        quote[30] = format!("{date}150000");
        serde_json::to_vec(&serde_json::json!({
            "code": 0, "data": {symbol: {
                "data": {"date": date, "data": rows}, "qt": {symbol: quote}
            }}
        }))
        .unwrap()
    }

    #[test]
    fn parses_prices_sorts_deduplicates_and_discards_invalid_or_out_of_session_points() {
        let data = payload(
            "sh600519",
            "20260918",
            &[
                "1500 10.40 20 2000",
                "0930 10.01 1 100",
                "0930 10.02 2 200",
                "1300 10.30",
                "1130 9.90",
                "1530 10.40",
                "1200 10.20",
                "0925 10",
                "2460 10",
                "0960 10",
                "中文 10",
                "0931 0",
                "0932 NaN",
                "0933 inf",
                "0934 -1",
                "0935 1e99",
                "invalid",
                "0936 missing",
            ],
        );
        let series = parse_intraday(&data, "600519").unwrap();
        assert_eq!(series.symbol, "sh600519");
        assert_eq!(series.name, "测试股票");
        assert_eq!(series.previous_close, Some(10.0));
        assert_eq!(
            series
                .points
                .iter()
                .map(|p| (p.minute, p.price))
                .collect::<Vec<_>>(),
            vec![(570, 10.02), (690, 9.9), (780, 10.3), (900, 10.4)]
        );
        assert_eq!(series.points[0].time_label(), "09:30");
    }

    #[test]
    fn hong_kong_retains_its_later_morning_and_closing_auction() {
        let data = payload(
            "hk00700",
            "20260918",
            &[
                "0930 428.000",
                "1159 423.200",
                "1200 423.400",
                "1230 422",
                "1300 422.800",
                "1559 421.400",
                "1608 419.000",
                "1611 419",
            ],
        );
        let mut root: Value = serde_json::from_slice(&data).unwrap();
        root["data"]["hk00700"]["qt"]["hk00700"][30] = "2026/09/18 16:08:32".into();
        let series = parse_intraday(&serde_json::to_vec(&root).unwrap(), "700.HK").unwrap();
        assert_eq!(series.previous_close, Some(10.0));
        assert_eq!(series.points.len(), 6);
        assert_eq!(series.points.last().unwrap().minute, 968);
        assert_eq!(
            session_position("hk00700", 720),
            session_position("hk00700", 780)
        );
        assert_eq!(session_position("hk00700", 970), Some(1.0));
        assert_eq!(
            session_position("sh600519", 690),
            session_position("sh600519", 780)
        );
        assert_eq!(session_position("sh600519", 900), Some(1.0));
        assert_eq!(session_position("sh600519", 570), Some(0.0));
        assert_eq!(session_position("sh600519", 720), None);
        assert_eq!(session_position("sh600519", 930), None);
    }

    #[test]
    fn date_mismatch_or_bad_previous_close_never_produces_a_misleading_baseline() {
        let mut root: Value =
            serde_json::from_slice(&payload("sh600519", "20260918", &["0930 10"])).unwrap();
        for stamp in ["20260917150000", "20260921100000", "", "invalid"] {
            root["data"]["sh600519"]["qt"]["sh600519"][30] = stamp.into();
            let series = parse_intraday(&serde_json::to_vec(&root).unwrap(), "sh600519").unwrap();
            assert_eq!(series.previous_close, None);
        }
        root["data"]["sh600519"]["qt"]["sh600519"][30] = "20260918150000".into();
        for price in ["0", "-1", "NaN", "inf", ""] {
            root["data"]["sh600519"]["qt"]["sh600519"][4] = price.into();
            assert_eq!(
                parse_intraday(&serde_json::to_vec(&root).unwrap(), "sh600519")
                    .unwrap()
                    .previous_close,
                None
            );
        }
    }

    #[test]
    fn empty_flat_and_single_point_sessions_have_finite_nonzero_ranges() {
        for rows in [vec![], vec!["0930 10"], vec!["0930 10", "1500 10"]] {
            let mut series =
                parse_intraday(&payload("sz000001", "20260918", &rows), "sz000001").unwrap();
            for baseline in [Some(10.0), None] {
                series.previous_close = baseline;
                let (low, high) = series.price_range();
                assert!(low.is_finite() && high.is_finite() && high > low);
                assert!(
                    series
                        .points
                        .iter()
                        .all(|point| (low..=high).contains(&point.price))
                );
            }
        }
        for data in [
            b"garbage".to_vec(),
            b"{\"code\":1}".to_vec(),
            payload("sz000001", "bad-date", &["0930 10"]),
            payload("sz000001", "20260918", &["0930 0"]),
        ] {
            assert!(parse_intraday(&data, "sz000001").is_err());
        }
        assert!(
            parse_intraday(&payload("sh600519", "20260918", &["0930 10"]), "sz000001").is_err()
        );
    }

    #[test]
    fn failed_or_older_responses_preserve_the_chart_and_new_trading_days_replace_it() {
        let state = Arc::new(Mutex::new(IntradaySnapshot {
            generation: 2,
            symbol: Some("sh600519".into()),
            ..Default::default()
        }));
        let request = Request {
            generation: 2,
            symbol: Some("sh600519".into()),
            paused: false,
        };
        let wake: Wake = Arc::new(|| {});
        let result = || {
            parse_intraday(
                &payload("sh600519", "20260918", &["0930 10", "1500 11"]),
                "sh600519",
            )
        };
        complete(&state, &request, result(), &wake);
        complete(&state, &request, Err("网络断开".into()), &wake);
        assert_eq!(
            state.lock().unwrap().series.as_ref().unwrap().points.len(),
            2
        );
        assert!(state.lock().unwrap().error.is_some());
        for (date, rows) in [
            ("20260918", vec!["0930 9"]),
            ("20260917", vec!["1500 8"]),
            ("20260918", vec![]),
        ] {
            complete(
                &state,
                &request,
                parse_intraday(&payload("sh600519", date, &rows), "sh600519"),
                &wake,
            );
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .series
                    .as_ref()
                    .unwrap()
                    .points
                    .last()
                    .unwrap()
                    .price,
                11.0
            );
        }
        complete(
            &state,
            &Request {
                generation: 1,
                ..request.clone()
            },
            Err("过期响应".into()),
            &wake,
        );
        assert_ne!(state.lock().unwrap().error.as_deref(), Some("过期响应"));
        complete(
            &state,
            &request,
            parse_intraday(&payload("sh600519", "20260921", &["0930 12"]), "sh600519"),
            &wake,
        );
        let snapshot = state.lock().unwrap();
        assert_eq!(snapshot.series.as_ref().unwrap().points.len(), 1);
        assert_eq!(snapshot.series.as_ref().unwrap().points[0].price, 12.0);
        assert!(snapshot.error.is_none());
    }

    #[test]
    fn refresh_rate_covers_hk_auctions_and_slows_down_during_lunch_and_weekends() {
        for (stamp, mainland, hk) in [
            ("2026-09-18T10:00:00+08:00", 15, 15),
            ("2026-09-18T11:50:00+08:00", 60, 15),
            ("2026-09-18T12:30:00+08:00", 60, 60),
            ("2026-09-18T16:08:00+08:00", 60, 15),
            ("2026-09-19T10:00:00+08:00", 60, 60),
        ] {
            let now = DateTime::parse_from_rfc3339(stamp)
                .unwrap()
                .with_timezone(&Utc);
            assert_eq!(refresh_interval("sh600519", now).as_secs(), mainland);
            assert_eq!(refresh_interval("hk00700", now).as_secs(), hk);
        }
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut buffer = [0; 1024];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0 && request.len() + count < 8192);
            request.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(request).unwrap()
    }

    #[tokio::test]
    async fn http_decodes_utf8_and_rejects_oversized_responses() {
        for oversized in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/minute", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                assert!(
                    read_request(&mut socket)
                        .await
                        .starts_with("GET /minute?code=hk00700 ")
                );
                let body = payload("hk00700", "20260918", &["0930 428.123"]);
                let size = if oversized {
                    MAX_RESPONSE_BYTES + 1
                } else {
                    body.len()
                };
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                if !oversized {
                    socket.write_all(&body).await.unwrap();
                }
            });
            let result = fetch_from(&http_client().unwrap(), &endpoint, "700.HK").await;
            if oversized {
                assert!(result.unwrap_err().contains("过大"));
            } else {
                let series = result.unwrap();
                assert_eq!(series.name, "测试股票");
                assert_eq!(series.points[0].price, 428.123);
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn switching_cancels_pending_http_and_pause_close_and_stop_do_not_wait_for_network() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/minute", listener.local_addr().unwrap());
        let (started, mut received) = tokio::sync::mpsc::channel(8);
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let started = started.clone();
                tokio::spawn(async move {
                    let request = read_request(&mut socket).await;
                    let symbol = if request.contains("code=sh600519") {
                        "sh600519"
                    } else {
                        "hk00700"
                    };
                    started.send(symbol).await.unwrap();
                    if symbol == "sh600519" {
                        let mut byte = [0];
                        let _ = socket.read(&mut byte).await;
                    } else {
                        let body = payload(symbol, "20260918", &["0930 428"]);
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                        socket.write_all(&body).await.unwrap();
                    }
                });
            }
        });
        let state = Arc::new(Mutex::new(IntradaySnapshot::default()));
        let (requests, receiver) = watch::channel(Request::default());
        let (stop_tx, stop_rx) = oneshot::channel();
        let worker = tokio::spawn(run_worker(
            receiver,
            stop_rx,
            state.clone(),
            Arc::new(|| {}),
            endpoint,
        ));
        let feed = IntradayFeed {
            requests,
            snapshot: state,
            stop: None,
            worker: None,
        };
        feed.request(Some("sh600519"), false);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap(),
            Some("sh600519")
        );
        feed.request(Some("hk00700"), false);
        tokio::time::timeout(Duration::from_secs(2), async {
            while feed.latest().series.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(feed.latest().series.as_ref().unwrap().symbol, "hk00700");
        feed.request(Some("hk00700"), true);
        assert!(!feed.latest().loading);
        assert!(feed.latest().series.is_some());
        assert_eq!(received.recv().await, Some("hk00700"));
        assert!(
            tokio::time::timeout(Duration::from_millis(80), received.recv())
                .await
                .is_err()
        );
        feed.request(None, false);
        assert!(feed.latest().series.is_none());
        feed.request(Some("sh600519"), false);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap(),
            Some("sh600519")
        );
        stop_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_millis(500), worker)
            .await
            .unwrap()
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    #[ignore = "requires Tencent internet access"]
    async fn live_mainland_index_etf_and_hong_kong_minutes() {
        let client = http_client().unwrap();
        for symbol in ["sh600519", "sz000001", "sh000001", "sh510300", "hk00700"] {
            let series = fetch_intraday(&client, symbol).await.unwrap();
            assert_eq!(series.symbol, symbol);
            assert!(
                !series.points.is_empty(),
                "{symbol} returned no minute history"
            );
            assert!(
                series.previous_close.is_some(),
                "{symbol} missing previous close"
            );
            println!(
                "{symbol}: {} {} points, {}–{}",
                series.date,
                series.points.len(),
                series.points[0].time_label(),
                series.points.last().unwrap().time_label()
            );
        }
    }
}
