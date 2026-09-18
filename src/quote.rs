use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    SinaWebSocket,
    TencentHttp,
    SinaHttp,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::SinaWebSocket => "新浪推送",
            Self::TencentHttp => "腾讯行情",
            Self::SinaHttp => "新浪备用",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Quote {
    pub symbol: String,
    pub name: String,
    pub price: Option<f64>,
    pub previous_close: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub volume_shares: Option<f64>,
    /// Turnover in the security's quote currency; no currency conversion.
    pub turnover: Option<f64>,
    pub quote_time: DateTime<FixedOffset>,
    pub received_at: DateTime<Utc>,
    pub source: Source,
}

impl Quote {
    pub fn change(&self) -> Option<f64> {
        Some(self.price? - self.previous_close?)
    }

    pub fn change_percent(&self) -> Option<f64> {
        Some(self.change()? / self.previous_close? * 100.0)
    }

    pub fn decimals(&self) -> usize {
        price_decimals(&self.symbol)
    }
}

pub fn price_decimals(symbol: &str) -> usize {
    if symbol.starts_with("hk")
        || symbol.starts_with("sh5")
        || symbol.starts_with("sz15")
        || symbol.starts_with("sz16")
    {
        3
    } else {
        2
    }
}

#[derive(Default, Debug)]
pub struct ParsedQuotes {
    pub quotes: Vec<Quote>,
    pub missing: Vec<String>,
    pub rejected: usize,
    pub server_error: Option<String>,
}

/// Restrict symbols before placing them in a URL or a subscription frame.
/// Six-digit input defaults to Shenzhen for 0/1/2/3 and Shanghai for 5/6/9.
/// Five-digit input is Hong Kong; an explicit HK prefix/suffix allows 1–5 digits.
pub fn normalize_symbol(input: &str) -> Result<String, String> {
    let value = input.trim().to_ascii_lowercase();
    let symbol = if let Some((code, market)) = value.split_once('.') {
        format!("{market}{code}")
    } else if value.len() == 5 && value.bytes().all(|b| b.is_ascii_digit()) {
        format!("hk{value}")
    } else if value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit()) {
        let market = match value.as_bytes()[0] {
            b'0'..=b'3' => "sz",
            b'5' | b'6' | b'9' => "sh",
            _ => return Err("目前支持沪深及港股，请检查代码和市场".into()),
        };
        format!("{market}{value}")
    } else {
        value
    };
    if let Some(code) = symbol.strip_prefix("hk")
        && (1..=5).contains(&code.len())
        && code.bytes().all(|byte| byte.is_ascii_digit())
    {
        Ok(format!("hk{code:0>5}"))
    } else if symbol.len() == 8
        && (symbol.starts_with("sh") || symbol.starts_with("sz"))
        && symbol.as_bytes()[2..].iter().all(u8::is_ascii_digit)
    {
        Ok(symbol)
    } else {
        Err("请输入 600519、sz000001、00700 或 700.HK 这样的代码".into())
    }
}

fn number(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn positive(value: &str) -> Option<f64> {
    number(value).filter(|v| *v > 0.0)
}

fn china_time(value: &str, format: &str) -> Option<DateTime<FixedOffset>> {
    let naive = NaiveDateTime::parse_from_str(value, format).ok()?;
    FixedOffset::east_opt(8 * 3600)?
        .from_local_datetime(&naive)
        .single()
}

fn normalize_sina_symbol(key: &str) -> Result<String, String> {
    // Sina's HK real-time channel uses an rt_ prefix; store one canonical code
    // so push and both HTTP sources update the same watchlist entry.
    normalize_symbol(
        key.strip_prefix("rt_")
            .filter(|key| key.starts_with("hk"))
            .unwrap_or(key),
    )
}

pub fn parse_sina(payload: &str, source: Source) -> ParsedQuotes {
    let mut result = ParsedQuotes::default();
    for line in payload.split(['\n', ';']) {
        let Some((key, raw)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key
            .trim()
            .trim_start_matches("var ")
            .trim_start_matches("hq_str_");
        let raw = raw.trim().trim_matches('"');
        if key == "sys_nxkey" {
            result
                .missing
                .extend(raw.split(',').filter_map(|s| normalize_sina_symbol(s).ok()));
            continue;
        }
        if key == "sys_auth" && (raw.contains("FAILED") || raw.contains("INVALID")) {
            result.server_error = Some("新浪推送暂时拒绝连接".into());
            continue;
        }
        if key.starts_with("sys_") {
            continue;
        }
        let Ok(symbol) = normalize_sina_symbol(key) else {
            continue;
        };
        if raw.is_empty() {
            result.missing.push(symbol);
            continue;
        }
        let fields: Vec<_> = raw.split(',').collect();
        let Some(quote) = sina_quote(symbol, &fields, source) else {
            result.rejected += 1;
            continue;
        };
        result.quotes.push(quote);
    }
    result
}

fn sina_quote(symbol: String, fields: &[&str], source: Source) -> Option<Quote> {
    if symbol.starts_with("hk") {
        return sina_hk_quote(symbol, fields, source);
    }
    if fields.len() < 32 || fields[0].trim().is_empty() {
        return None;
    }
    // A zero current price is common before the first trade or during suspension.
    // Preserve it as unavailable instead of displaying a fictitious -100% change.
    number(fields[3])?;
    number(fields[2])?;
    let quote_time = china_time(
        &format!("{} {}", fields[30], fields[31]),
        "%Y-%m-%d %H:%M:%S",
    )?;
    Some(Quote {
        symbol,
        name: fields[0].trim().to_owned(),
        price: positive(fields[3]),
        previous_close: positive(fields[2]),
        open: positive(fields[1]),
        high: positive(fields[4]),
        low: positive(fields[5]),
        volume_shares: number(fields[8]),
        turnover: number(fields[9]),
        quote_time,
        received_at: Utc::now(),
        source,
    })
}

fn sina_hk_quote(symbol: String, fields: &[&str], source: Source) -> Option<Quote> {
    if fields.len() < 19 || fields[1].trim().is_empty() {
        return None;
    }
    number(fields[6])?;
    number(fields[3])?;
    let time = format!("{} {}", fields[17], fields[18]);
    let quote_time =
        china_time(&time, "%Y/%m/%d %H:%M:%S").or_else(|| china_time(&time, "%Y/%m/%d %H:%M"))?;
    Some(Quote {
        symbol,
        name: fields[1].trim().to_owned(),
        price: positive(fields[6]),
        previous_close: positive(fields[3]),
        open: positive(fields[2]),
        high: positive(fields[4]),
        low: positive(fields[5]),
        volume_shares: number(fields[12]),
        turnover: number(fields[11]),
        quote_time,
        received_at: Utc::now(),
        source,
    })
}

pub fn parse_tencent(payload: &str) -> ParsedQuotes {
    let mut result = ParsedQuotes::default();
    for line in payload.split(['\n', ';']) {
        let Some((key, raw)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key.trim().trim_start_matches("v_");
        let Ok(symbol) = normalize_symbol(key) else {
            continue;
        };
        let raw = raw.trim().trim_matches('"');
        if raw.is_empty() {
            result.missing.push(symbol);
            continue;
        }
        let fields: Vec<_> = raw.split('~').collect();
        let Some(quote) = tencent_quote(symbol, &fields) else {
            result.rejected += 1;
            continue;
        };
        result.quotes.push(quote);
    }
    result
}

fn tencent_quote(symbol: String, fields: &[&str]) -> Option<Quote> {
    if fields.len() < 38 || fields[1].trim().is_empty() {
        return None;
    }
    number(fields[3])?;
    number(fields[4])?;
    let hong_kong = symbol.starts_with("hk");
    let quote_time = china_time(
        fields[30],
        if hong_kong {
            "%Y/%m/%d %H:%M:%S"
        } else {
            "%Y%m%d%H%M%S"
        },
    )?;
    Some(Quote {
        symbol,
        name: fields[1].trim().to_owned(),
        price: positive(fields[3]),
        previous_close: positive(fields[4]),
        open: positive(fields[5]),
        high: positive(fields[33]),
        low: positive(fields[34]),
        // A-shares use lots / ten-thousand yuan; HK uses shares / quote currency.
        volume_shares: number(fields[6]).map(|v| v * if hong_kong { 1.0 } else { 100.0 }),
        turnover: number(fields[37]).map(|v| v * if hong_kong { 1.0 } else { 10_000.0 }),
        quote_time,
        received_at: Utc::now(),
        source: Source::TencentHttp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SINA: &str = "sh600519=贵州茅台,1262.990,1266.980,1257.120,1265.880,1256.100,1257.120,1257.130,2489087,3135849108.000,831,1257.120,200,1257.110,100,1257.080,200,1257.060,200,1257.050,100,1257.130,200,1257.240,100,1257.280,1600,1258.000,100,1258.280,2026-09-18,15:34:59,00,D|4200|5279904.00";

    #[test]
    fn normalizes_market_codes_without_allowing_protocol_injection() {
        for (input, expected) in [
            ("600519", "sh600519"),
            ("000001", "sz000001"),
            (" SH000001 ", "sh000001"),
            ("600519.SH", "sh600519"),
            ("159915", "sz159915"),
            ("00700", "hk00700"),
            (" HK00700 ", "hk00700"),
            ("hk700", "hk00700"),
            ("700.HK", "hk00700"),
            ("0700.hk", "hk00700"),
            ("00700.HK", "hk00700"),
            ("09988", "hk09988"),
        ] {
            assert_eq!(normalize_symbol(input).unwrap(), expected);
        }
        for input in [
            "",
            "sh600519,sz000001",
            "sh600519\n=sz000001",
            "sh00000x",
            "😀600519",
            "830799",
            "hk",
            "hk123456",
            "hk00x00",
            "hk00700,sh600519",
            "700.HK?x=y",
            "700.hk.sh",
            "hk７００",
            "rt_hk00700",
        ] {
            assert!(normalize_symbol(input).is_err(), "{input}");
        }
    }

    #[test]
    fn parses_websocket_and_http_using_the_same_fields() {
        let ws = parse_sina(
            &format!("sys_ver=1\n{SINA}\nsys_nxkey=sz999999\n"),
            Source::SinaWebSocket,
        );
        let http = parse_sina(
            &format!("var hq_str_{};", SINA.replacen('=', "=\"", 1) + "\""),
            Source::SinaHttp,
        );
        assert_eq!(ws.quotes.len(), 1);
        assert_eq!(ws.missing, ["sz999999"]);
        assert_eq!(ws.quotes[0].price, Some(1257.12));
        assert_eq!(http.quotes[0].price, ws.quotes[0].price);
        assert_eq!(ws.quotes[0].quote_time.offset().local_minus_utc(), 28800);
        assert!((ws.quotes[0].change_percent().unwrap() + 0.77823).abs() < 0.001);
    }

    #[test]
    fn missing_and_malformed_rows_do_not_discard_good_rows() {
        let payload = format!("sh600000=bad\nvar hq_str_sz000001=\"\";\n{SINA}");
        let result = parse_sina(&payload, Source::SinaWebSocket);
        assert_eq!(result.quotes.len(), 1);
        assert_eq!(result.rejected, 1);
        assert_eq!(result.missing, ["sz000001"]);
        for broken in [
            SINA.replace("1257.120", "NaN"),
            SINA.replace("2026-09-18", "not-a-date"),
        ] {
            assert!(parse_sina(&broken, Source::SinaWebSocket).quotes.is_empty());
        }
    }

    #[test]
    fn suspended_quotes_do_not_show_minus_one_hundred_percent() {
        let result = parse_sina(&SINA.replace("1257.120", "0.000"), Source::SinaWebSocket);
        assert_eq!(result.quotes[0].price, None);
        assert_eq!(result.quotes[0].change_percent(), None);
    }

    #[test]
    fn tencent_units_and_timestamp_are_normalized() {
        let mut fields = vec!["0"; 38];
        fields[1] = "贵州茅台";
        fields[3] = "1257.12";
        fields[4] = "1266.98";
        fields[6] = "24891";
        fields[30] = "20260918161436";
        fields[37] = "313585";
        let parsed = parse_tencent(&format!("v_sh600519=\"{}\";", fields.join("~")));
        let quote = &parsed.quotes[0];
        assert_eq!(quote.volume_shares, Some(2_489_100.0));
        assert_eq!(quote.turnover, Some(3_135_850_000.0));
        assert_eq!(quote.quote_time.format("%H:%M:%S").to_string(), "16:14:36");
    }

    const SINA_HK: &str = "rt_hk00700=TENCENT,腾讯控股,428.000,426.000,430.400,419.000,419.000,-7.000,-1.643,418.800,419.000,12180786280.956,28796138,15.229,0.000,675.134,411.000,2026/09/18,16:08:32,100|0,N|Y|Y";

    #[test]
    fn parses_mixed_sina_markets_and_canonicalizes_hk_channel_names() {
        for source in [Source::SinaWebSocket, Source::SinaHttp] {
            let result = parse_sina(
                &format!(
                    "{SINA}\nvar hq_str_{};\nsys_nxkey=rt_hk09999",
                    SINA_HK.replacen('=', "=\"", 1) + "\""
                ),
                source,
            );
            assert_eq!(result.quotes.len(), 2);
            assert_eq!(result.rejected, 0);
            assert_eq!(result.missing, ["hk09999"]);
            let hk = &result.quotes[1];
            assert_eq!(hk.symbol, "hk00700");
            assert_eq!(hk.name, "腾讯控股");
            assert_eq!(hk.price, Some(419.0));
            assert_eq!(hk.previous_close, Some(426.0));
            assert_eq!(hk.open, Some(428.0));
            assert_eq!(hk.high, Some(430.4));
            assert_eq!(hk.low, Some(419.0));
            assert_eq!(hk.volume_shares, Some(28_796_138.0));
            assert_eq!(hk.turnover, Some(12_180_786_280.956));
            assert_eq!(
                hk.quote_time.format("%H:%M:%S %z").to_string(),
                "16:08:32 +0800"
            );
            assert_eq!(hk.decimals(), 3);
            assert_eq!(hk.source, source);
        }
    }

    #[test]
    fn hk_minute_timestamps_empty_and_suspended_quotes_are_handled() {
        let delayed = SINA_HK.replace("rt_hk", "hk").replace("16:08:32", "16:08");
        let quote = &parse_sina(&delayed, Source::SinaHttp).quotes[0];
        assert_eq!(quote.quote_time.format("%H:%M:%S").to_string(), "16:08:00");
        let suspended = SINA_HK.replace("419.000", "0.000");
        assert_eq!(
            parse_sina(&suspended, Source::SinaHttp).quotes[0].change_percent(),
            None
        );
        let invalid = format!(
            "rt_hk00001=bad\nrt_hk00002=\n{}",
            SINA_HK.replace("2026/09/18", "bad-date")
        );
        let parsed = parse_sina(&invalid, Source::SinaHttp);
        assert_eq!(parsed.rejected, 2);
        assert_eq!(parsed.missing, ["hk00002"]);
        assert!(parsed.quotes.is_empty());
    }

    #[test]
    fn tencent_hk_volume_and_turnover_do_not_use_mainland_multipliers() {
        let mut fields = vec!["0"; 38];
        fields[1] = "小米集团-W";
        fields[3] = "26.401";
        fields[4] = "26.400";
        fields[6] = "109848697.0";
        fields[30] = "2026/09/18 16:08:41";
        fields[37] = "2889628005.910";
        let payload = format!("v_hk01810=\"{}\";", fields.join("~"));
        let parsed = parse_tencent(&payload);
        let quote = &parsed.quotes[0];
        assert_eq!(quote.symbol, "hk01810");
        assert_eq!(quote.volume_shares, Some(109_848_697.0));
        assert_eq!(quote.turnover, Some(2_889_628_005.91));
        assert_eq!(
            format!("{:.*}", quote.decimals(), quote.price.unwrap()),
            "26.401"
        );
        assert_eq!(
            format!("{:+.*}", quote.decimals(), quote.change().unwrap()),
            "+0.001"
        );
        assert_eq!(quote.quote_time.format("%H:%M:%S").to_string(), "16:08:41");
        assert!(
            parse_tencent(&payload.replace("26.401", "NaN"))
                .quotes
                .is_empty()
        );
        assert_eq!(
            parse_tencent(&payload.replace("26.401", "0.000")).quotes[0].price,
            None
        );
    }
}
