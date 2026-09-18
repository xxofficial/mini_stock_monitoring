use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use tokio::sync::{oneshot, watch};

use crate::{
    feed::{Wake, http_client},
    quote::normalize_symbol,
};

const ENDPOINT: &str = "https://suggest3.sinajs.cn/suggest/type=11,12,13,14,15,31,203&key=";
const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub const MAX_QUERY_CHARS: usize = 64;
pub const MAX_RESULTS: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockMatch {
    pub symbol: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct SearchSnapshot {
    generation: u64,
    pub query: String,
    pub loading: bool,
    pub matches: Vec<StockMatch>,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
struct SearchRequest {
    generation: u64,
    query: String,
}

type SharedSearch = Arc<Mutex<SearchSnapshot>>;

/// A single cancellable worker; typing never blocks the UI or queues old searches.
pub struct StockSearch {
    requests: watch::Sender<SearchRequest>,
    snapshot: SharedSearch,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl StockSearch {
    pub fn spawn(wake: Wake) -> std::io::Result<Self> {
        Self::spawn_with_endpoint(wake, ENDPOINT.into())
    }

    fn spawn_with_endpoint(wake: Wake, endpoint: String) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let snapshot = Arc::new(Mutex::new(SearchSnapshot::default()));
        let shared = snapshot.clone();
        let (requests, receiver) = watch::channel(SearchRequest::default());
        let (stop, stop_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("stock-search".into())
            .spawn(move || {
                runtime.block_on(run_worker(receiver, stop_rx, shared, wake, endpoint));
            })?;
        Ok(Self {
            requests,
            snapshot,
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    /// Empty input cancels any pending request and clears its results immediately.
    pub fn request(&self, input: &str) {
        if let Ok(mut state) = self.snapshot.lock() {
            let query = input.trim().to_ascii_lowercase();
            let too_long = query.chars().count() > MAX_QUERY_CHARS;
            let generation = state.generation.wrapping_add(1);
            *state = SearchSnapshot {
                generation,
                loading: !query.is_empty() && !too_long,
                query: query.clone(),
                error: too_long.then(|| format!("搜索内容请控制在 {MAX_QUERY_CHARS} 字以内")),
                ..SearchSnapshot::default()
            };
            self.requests.send_replace(SearchRequest {
                generation,
                query: if too_long { String::new() } else { query },
            });
        }
    }

    pub fn latest(&self) -> SearchSnapshot {
        self.snapshot
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

impl Drop for StockSearch {
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
    mut requests: watch::Receiver<SearchRequest>,
    mut stop: oneshot::Receiver<()>,
    state: SharedSearch,
    wake: Wake,
    endpoint: String,
) {
    let client = http_client();
    loop {
        let current = requests.borrow_and_update().clone();
        tokio::select! {
            biased;
            _ = &mut stop => return,
            changed = requests.changed() => {
                if changed.is_err() { return; }
            },
            _ = async {
                if !current.query.is_empty() {
                    tokio::time::sleep(DEBOUNCE).await;
                    let result = match &client {
                        Ok(client) => fetch_matches(client, &endpoint, &current.query).await,
                        Err(_) => Err("名称搜索暂不可用，请使用股票代码添加".into()),
                    };
                    complete_search(&state, &current, result, &wake);
                }
                std::future::pending::<()>().await;
            } => {},
        }
    }
}

fn complete_search(
    state: &SharedSearch,
    request: &SearchRequest,
    result: Result<Vec<StockMatch>, String>,
    wake: &Wake,
) {
    if let Ok(mut state) = state.lock() {
        // Also guard publication: a new query can arrive as HTTP finishes.
        if state.generation != request.generation {
            return;
        }
        state.loading = false;
        match result {
            Ok(matches) => {
                state.matches = matches;
                state.error = None;
            }
            Err(error) => {
                state.matches.clear();
                state.error = Some(error);
            }
        }
    }
    wake();
}

async fn fetch_matches(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
) -> Result<Vec<StockMatch>, String> {
    // This service uses path parameters, not a URL query string. Encode every
    // non-unreserved UTF-8 byte so input cannot add parameters or fragments.
    // Sina search uses bare five-digit HK codes (hk00700 returns no results).
    let key = match normalize_symbol(query) {
        Ok(symbol) if symbol.starts_with("hk") => symbol[2..].to_owned(),
        Ok(symbol) => symbol,
        Err(_) => query.to_owned(),
    };
    let encoded: String = key
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect();
    let mut response = client
        .get(format!("{endpoint}{encoded}&name=suggestvalue"))
        .header("Referer", "https://finance.sina.com.cn/")
        .send()
        .await
        .map_err(|_| "名称搜索连接失败，请重试或输入股票代码".to_owned())?
        .error_for_status()
        .map_err(|_| "名称搜索服务暂不可用，请稍后重试".to_owned())?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err("搜索响应过大，请缩小查询范围".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "搜索结果读取失败，请重试".to_owned())?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("搜索响应过大，请缩小查询范围".into());
        }
        body.extend_from_slice(&chunk);
    }
    let (text, _, malformed) = encoding_rs::GB18030.decode(&body);
    if malformed {
        return Err("搜索结果文字编码异常，请稍后重试".into());
    }
    parse_matches(&text, query)
}

fn parse_matches(text: &str, query: &str) -> Result<Vec<StockMatch>, String> {
    let invalid = || "搜索结果格式异常，请稍后重试".to_owned();
    let (variable, value) = text.trim().split_once('=').ok_or_else(invalid)?;
    if variable.trim() != "var suggestvalue" {
        return Err(invalid());
    }
    // Parse the quoted data string, never execute the returned JavaScript.
    let data: String =
        serde_json::from_str(value.trim().trim_end_matches(';')).map_err(|_| invalid())?;
    let mut seen = BTreeSet::new();
    let mut matches = Vec::new();
    for row in data.split(';').filter(|row| !row.is_empty()) {
        let fields: Vec<_> = row.split(',').collect();
        if fields.len() < 5 || !matches!(fields[1], "11" | "12" | "13" | "14" | "15" | "31" | "203")
        {
            continue;
        }
        let raw_symbol = fields[3].trim();
        let symbol = if fields[1] == "31" {
            // Type 31 is HK equity. Only this type may turn a bare code into HK.
            if raw_symbol.len() != 5 || !raw_symbol.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            normalize_symbol(&format!("hk{raw_symbol}"))
        } else {
            // Do not infer a mainland market for funds, US stocks or bare codes.
            if !(raw_symbol.starts_with("sh") || raw_symbol.starts_with("sz")) {
                continue;
            }
            normalize_symbol(raw_symbol)
        };
        let Ok(symbol) = symbol else {
            continue;
        };
        let name = fields[4].trim();
        if fields[2] != &symbol[2..]
            || name.is_empty()
            || name.chars().count() > 80
            || name.chars().any(char::is_control)
            || !seen.insert(symbol.clone())
        {
            continue;
        }
        matches.push(StockMatch {
            symbol,
            name: name.into(),
        });
    }
    let query = query.trim().to_ascii_lowercase();
    let exact_symbol = normalize_symbol(&query).ok();
    matches.sort_by_key(|item| {
        let name = item.name.to_ascii_lowercase();
        if name == query || exact_symbol.as_ref() == Some(&item.symbol) {
            0
        } else if name.starts_with(&query) {
            1
        } else if name.contains(&query) {
            2
        } else {
            3
        }
    });
    matches.truncate(MAX_RESULTS);
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const MAOTAI: &str = "贵州茅台,11,600519,sh600519,贵州茅台,,贵州茅台,99,1,ESG,,";
    const PINGAN: &str = "平安银行,11,000001,sz000001,平安银行,,平安银行,99,1,ESG,,";
    const INDEX: &str = "上证指数,11,000001,sh000001,上证指数,,上证指数,99,1,,,";

    fn response(rows: &str) -> String {
        format!("var suggestvalue={};", serde_json::to_string(rows).unwrap())
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut buffer = [0; 1024];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0 && request.len() + count <= 8192);
            request.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(request).unwrap()
    }

    #[test]
    fn parses_partial_names_pinyin_and_ambiguous_codes() {
        assert_eq!(
            parse_matches(&response(MAOTAI), "茅台").unwrap()[0].name,
            "贵州茅台"
        );
        assert_eq!(
            parse_matches(&response(MAOTAI), "GZMT").unwrap()[0].symbol,
            "sh600519"
        );
        let rows = response(&format!("{INDEX};{PINGAN}"));
        assert_eq!(
            parse_matches(&rows, "000001").unwrap()[0].symbol,
            "sz000001"
        );
        assert_eq!(parse_matches(&rows, "上证").unwrap()[0].symbol, "sh000001");
    }

    #[test]
    fn ranks_exact_names_first_and_keeps_provider_order_for_equal_matches() {
        let rows = response(&format!("{MAOTAI};{PINGAN};{INDEX}"));
        let matches = parse_matches(&rows, "平安银行").unwrap();
        assert_eq!(matches[0].symbol, "sz000001");
        assert_eq!(matches[1].symbol, "sh600519");
        assert_eq!(matches[2].symbol, "sh000001");
    }

    #[test]
    fn filters_unsupported_and_invalid_symbols_and_deduplicates() {
        let rows = response(&format!(
            "{MAOTAI};{MAOTAI};腾讯,31,00700,00700,腾讯控股;基金,22,510300,of510300,沪深300ETF;ETF,203,510300,sh510300,沪深300ETF;坏,11,600519,sh600519&x=y,坏;错,11,000001,sz000002,错;空,11,000003,sz000003,;北,11,920001,bj920001,北交所;短行"
        ));
        let matches = parse_matches(&rows, "").unwrap();
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[1].symbol, "hk00700");
        assert_eq!(matches[2].symbol, "sh510300");
    }

    #[test]
    fn hk_results_use_five_digit_codes_and_keep_cross_listed_markets_distinct() {
        let rows = response(
            "中国平安,11,601318,sh601318,中国平安;中国平安,31,02318,02318,中国平安;腾讯控股,31,00700,00700,腾讯控股;重复,31,00700,00700,腾讯控股;错误,31,600519,600519,错误;错误,31,00700,00701,错误;美股,41,00700,00700,错误;注入,31,00700,00700&x=y,错误",
        );
        let matches = parse_matches(&rows, "700.HK").unwrap();
        assert_eq!(
            matches
                .iter()
                .map(|item| item.symbol.as_str())
                .collect::<Vec<_>>(),
            ["hk00700", "sh601318", "hk02318"]
        );
    }

    #[test]
    fn distinguishes_empty_results_from_invalid_responses() {
        assert!(
            parse_matches("var suggestvalue=\"\";", "不存在")
                .unwrap()
                .is_empty()
        );
        for invalid in [
            "",
            "<html>error</html>",
            "var wrong=\"\";",
            "var suggestvalue=broken;",
            "var suggestvalue=\"\";alert(1)",
        ] {
            assert!(parse_matches(invalid, "茅台").is_err());
        }
    }

    #[test]
    fn late_response_cannot_restore_an_old_query_even_when_query_text_repeats() {
        let state = Arc::new(Mutex::new(SearchSnapshot {
            generation: 3,
            query: "茅台".into(),
            loading: true,
            ..SearchSnapshot::default()
        }));
        complete_search(
            &state,
            &SearchRequest {
                generation: 1,
                query: "茅台".into(),
            },
            parse_matches(&response(MAOTAI), "茅台"),
            &(Arc::new(|| {}) as Wake),
        );
        assert!(state.lock().unwrap().loading);
        assert!(state.lock().unwrap().matches.is_empty());
    }

    async fn wait_finished(search: &StockSearch) -> SearchSnapshot {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = search.latest();
                if !snapshot.loading {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn debounces_input_encodes_query_and_decodes_chinese_results() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/suggest/key=", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .starts_with("GET /suggest/key=%E8%8C%85%E5%8F%B0%26%23%3F&name=suggestvalue "),
                "{request}"
            );
            let text = response(MAOTAI);
            let (body, _, _) = encoding_rs::GB18030.encode(&text);
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
        let search = StockSearch::spawn_with_endpoint(Arc::new(|| {}), endpoint).unwrap();
        search.request("银行");
        search.request(" 茅台&#? ");
        let snapshot = wait_finished(&search).await;
        assert_eq!(snapshot.error, None);
        assert_eq!(snapshot.matches[0].name, "贵州茅台");
        server.await.unwrap();
        search.request("");
        assert!(search.latest().matches.is_empty());
        assert!(!search.latest().loading);
    }

    #[tokio::test]
    async fn cancels_in_flight_search_and_stops_without_waiting_for_http_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/suggest/key=", listener.local_addr().unwrap());
        let search = StockSearch::spawn_with_endpoint(Arc::new(|| {}), endpoint).unwrap();
        search.request("茅台");
        let (mut first, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let first_request = read_request(&mut first).await;
        assert!(first_request.contains("%E8%8C%85%E5%8F%B0"));
        search.request("银行");
        assert!(search.latest().matches.is_empty());
        let (mut second, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let second_request = read_request(&mut second).await;
        assert!(second_request.contains("%E9%93%B6%E8%A1%8C"));
        let started = std::time::Instant::now();
        drop(search);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn http_errors_are_visible_and_retry_can_succeed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/suggest/key=", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for reply in [
                "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned(),
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response("").len(),
                    response("")
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                assert!(request.starts_with("GET "));
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let search = StockSearch::spawn_with_endpoint(Arc::new(|| {}), endpoint).unwrap();
        search.request("茅台");
        assert!(wait_finished(&search).await.error.is_some());
        search.request("茅台");
        let snapshot = wait_finished(&search).await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.matches.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "live Sina search service; run explicitly with network access"]
    async fn live_mainland_and_hong_kong_search() {
        let client = http_client().unwrap();
        for (query, expected) in [
            ("茅台", "sh600519"),
            ("银行", "sz000001"),
            ("gzmt", "sh600519"),
            ("上证", "sh000001"),
            ("300ETF", "sh510300"),
            ("600519.SH", "sh600519"),
            ("腾讯", "hk00700"),
            ("小米", "hk01810"),
            ("txkg", "hk00700"),
            ("hk00700", "hk00700"),
            ("700.HK", "hk00700"),
            ("09988", "hk09988"),
        ] {
            let matches = fetch_matches(&client, ENDPOINT, query).await.unwrap();
            assert!(
                matches.iter().any(|item| item.symbol == expected),
                "{query}: {matches:?}"
            );
        }
    }
}
