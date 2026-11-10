use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ai_json_filler_core::{BoxFuture, FillError, Tool, ToolOutput, ToolSpec};
use serde_json::{Value, json};

use crate::http::{build_client, read_json, to_tool_error};

const TAVILY_EXTRACT_URL: &str = "https://api.tavily.com/extract";
const MAX_URL_BYTES: usize = 2048;

pub struct FetchTool {
    name: String,
    client: FetchClient,
    max_chars: usize,
    allow_private: bool,
    timeout_secs: u64,
}

enum FetchClient {
    Crawl4ai { base_url: String },
    Tavily { api_key: String },
}

impl FetchTool {
    pub fn crawl4ai(base_url: impl Into<String>) -> Self {
        Self::new(
            "web_fetch_crawl4ai",
            FetchClient::Crawl4ai {
                base_url: base_url.into().trim_end_matches('/').to_string(),
            },
        )
    }

    pub fn tavily(api_key: impl Into<String>) -> Self {
        Self::new(
            "web_fetch_tavily",
            FetchClient::Tavily {
                api_key: api_key.into(),
            },
        )
    }

    pub fn default_crawl4ai(base_url: impl Into<String>) -> Self {
        Self {
            name: "web_fetch".to_string(),
            ..Self::crawl4ai(base_url)
        }
    }

    pub fn default_tavily(api_key: impl Into<String>) -> Self {
        Self {
            name: "web_fetch".to_string(),
            ..Self::tavily(api_key)
        }
    }

    fn new(name: &str, client: FetchClient) -> Self {
        Self {
            name: name.to_string(),
            client,
            max_chars: 20_000,
            allow_private: false,
            timeout_secs: 30,
        }
    }

    pub fn with_max_chars(mut self, n: usize) -> Self {
        self.max_chars = n.max(1);
        self
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs.max(1);
        self
    }

    pub fn allow_private_hosts(mut self, yes: bool) -> Self {
        self.allow_private = yes;
        self
    }
}

impl Tool for FetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "Web のページを取得し、本文の markdown を返す。検索結果の抜粋で足りないときだけ使う。本文が長い場合は next_offset を渡して続きを取得できる。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) の絶対 URL" },
                    "max_chars": { "type": "integer", "description": "本文の上限文字数（既定20000）" },
                    "offset": { "type": "integer", "description": "続きを読む開始位置（文字数）" }
                },
                "required": ["url"]
            }),
        }
    }

    fn prompt_hint(&self) -> Option<&str> {
        Some(
            "【ページ取得の方針】\n\
             - 検索結果の抜粋で足りないときだけ使う\n\
             - 取得は合計3回まで\n\
             - 本文が長くて truncated=true なら next_offset を offset に渡す\n\
             - 取得に失敗したら同じ URL を繰り返さず、別の情報源に切り替える",
        )
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
        Box::pin(async move { self.run(args).await })
    }
}

impl FetchTool {
    async fn run(&self, args: Value) -> Result<ToolOutput, FillError> {
        let url = args.get("url").and_then(Value::as_str).unwrap_or("").trim();
        if url.is_empty() {
            return Ok(ToolOutput::error("FETCH_ERROR: url が空です"));
        }
        if let Some(reason) = validate_url(url, self.allow_private) {
            return Ok(ToolOutput::error(format!("FETCH_ERROR: {reason}")));
        }
        let max_chars = args
            .get("max_chars")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(self.max_chars)
            .clamp(1, 100_000);
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;

        match &self.client {
            FetchClient::Crawl4ai { base_url } => {
                let body = json!({ "url": url, "f": "fit" });
                let response = build_client(self.timeout_secs)
                    .post(format!("{base_url}/md"))
                    .json(&body)
                    .send()
                    .await;
                let value = match read_json(response).await {
                    Ok(value) => value,
                    Err(message) => {
                        return Ok(ToolOutput::error(format!("FETCH_ERROR: {message}")));
                    }
                };
                let markdown = parse_crawl4ai(&value)?;
                let (content, truncated, next_offset) = slice_content(&markdown, offset, max_chars);
                serialize(&self.name, url, &content, truncated, next_offset)
            }
            FetchClient::Tavily { api_key } => {
                let body = json!({ "urls": [url] });
                let response = build_client(self.timeout_secs)
                    .post(TAVILY_EXTRACT_URL)
                    .bearer_auth(api_key)
                    .json(&body)
                    .send()
                    .await;
                let value = match read_json(response).await {
                    Ok(value) => value,
                    Err(message) => {
                        return Ok(ToolOutput::error(format!("FETCH_ERROR: {message}")));
                    }
                };
                let markdown = parse_tavily_extract(&value)?;
                let (content, truncated, next_offset) = slice_content(&markdown, offset, max_chars);
                serialize(&self.name, url, &content, truncated, next_offset)
            }
        }
    }
}

/// Validate a URL for SSRF protection.
///
/// Returns `Some(message)` when the URL is rejected, or `None` when the URL is
/// acceptable. Hostname resolution is deliberately not performed; only the
/// syntactic form is checked (with the well-known exception of the literal
/// hostname `localhost`, which is rejected).
pub fn validate_url(url: &str, allow_private: bool) -> Option<String> {
    if url.len() > MAX_URL_BYTES {
        return Some(format!("URL が長すぎます (上限 {MAX_URL_BYTES} bytes)"));
    }
    let parsed = match reqwest::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(e) => return Some(format!("URL を解析できません: {e}")),
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Some("http/https 以外の scheme は拒否します".to_string());
    }

    let Some(raw_host) = parsed.host_str() else {
        return Some("URL にホスト名がありません".to_string());
    };
    // IPv6 hosts come back bracketed (`[::1]`), and a trailing dot is a valid
    // FQDN form (`localhost.`) that must not slip past the localhost check.
    let host = raw_host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_lowercase();
    if host.is_empty() {
        return Some("URL にホスト名がありません".to_string());
    }

    if host == "localhost" && !allow_private {
        return Some("localhost は拒否します".to_string());
    }

    if let Some(ip) = parse_ip(&host)
        && let Some(message) = check_ip(&ip, allow_private)
    {
        return Some(message);
    }

    None
}

fn parse_ip(host: &str) -> Option<IpAddr> {
    // Try IPv6 first so that `::ffff:127.0.0.1` is recognised and collapsed to
    // its IPv4-mapped form before the check below.
    if let Ok(v6) = host.parse::<Ipv6Addr>() {
        return match v6.to_ipv4_mapped() {
            Some(v4) => Some(IpAddr::V4(v4)),
            None => Some(IpAddr::V6(v6)),
        };
    }
    host.parse::<Ipv4Addr>().ok().map(IpAddr::V4)
}

fn check_ip(ip: &IpAddr, allow_private: bool) -> Option<String> {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, _, _] = v4.octets();
            // 100.64.0.0/10 (CGNAT)
            if a == 100 && (64..=127).contains(&b) {
                return Some(format!("CGNAT アドレス {v4} は拒否します"));
            }
            if v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (v4.is_private() && !allow_private)
            {
                return Some(format!(
                    "プライベート/ループバックアドレス {v4} は拒否します"
                ));
            }
            None
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return Some(format!("ループバック/未指定アドレス {v6} は拒否します"));
            }
            let o = v6.octets();
            // fc00::/7 (unique local)
            if (o[0] & 0xfe) == 0xfc {
                return Some(format!("ローカルアドレス {v6} は拒否します"));
            }
            // fe80::/10 (link local)
            if o[0] == 0xfe && (o[1] & 0xc0) == 0x80 {
                return Some(format!("リンクローカルアドレス {v6} は拒否します"));
            }
            None
        }
    }
}

/// Slice `content` by characters, starting at `offset`.
///
/// Returns `(part, truncated, next_offset)`. `next_offset` is only set when
/// characters remain after the returned slice.
pub fn slice_content(
    content: &str,
    offset: usize,
    max_chars: usize,
) -> (String, bool, Option<usize>) {
    let total = content.chars().count();
    let start = offset.min(total);
    let max_chars = max_chars.max(1);
    let part: String = content.chars().skip(start).take(max_chars).collect();
    let end = start + part.chars().count();
    if end >= total {
        (part, false, None)
    } else {
        (part, true, Some(end))
    }
}

pub(crate) fn parse_crawl4ai(value: &Value) -> Result<String, FillError> {
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        let detail = value
            .get("error")
            .or_else(|| value.get("detail"))
            .and_then(Value::as_str)
            .unwrap_or("不明");
        return Err(to_tool_error(
            "web_fetch",
            format!("FETCH_ERROR: Crawl4AI が失敗を返しました: {detail}"),
        ));
    }
    let markdown = value
        .get("markdown")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if markdown.is_empty() {
        return Err(to_tool_error(
            "web_fetch",
            "FETCH_ERROR: Crawl4AI の応答に markdown がありません",
        ));
    }
    Ok(markdown.to_string())
}

pub(crate) fn parse_tavily_extract(value: &Value) -> Result<String, FillError> {
    let raw = value
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| results.first())
        .and_then(|first| first.get("raw_content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if raw.is_empty() {
        return Err(to_tool_error(
            "web_fetch",
            "FETCH_ERROR: Tavily の extract 応答に results[0].raw_content がありません",
        ));
    }
    Ok(raw.to_string())
}

fn serialize(
    name: &str,
    url: &str,
    content: &str,
    truncated: bool,
    next_offset: Option<usize>,
) -> Result<ToolOutput, FillError> {
    let value = json!({
        "url": url,
        "content": content,
        "truncated": truncated,
        "next_offset": next_offset,
    });
    match serde_json::to_string(&value) {
        Ok(s) => Ok(ToolOutput::success(s)),
        Err(e) => Err(to_tool_error(name, format!("FETCH_ERROR: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crawl4ai_success_false_is_error() {
        let err = parse_crawl4ai(&json!({ "success": false, "error": "x" })).unwrap_err();
        assert!(matches!(err, FillError::Tool { .. }));
    }

    #[test]
    fn tavily_extract_reads_raw_content() {
        let out = parse_tavily_extract(&json!({ "results": [{ "raw_content": "abc" }] })).unwrap();
        assert_eq!(out, "abc");
    }

    #[test]
    fn rejects_loopback() {
        assert!(validate_url("http://127.0.0.1/", false).is_some());
    }

    #[test]
    fn rejects_private() {
        assert!(validate_url("http://10.0.0.1/", false).is_some());
        assert!(validate_url("http://192.168.1.1/", false).is_some());
        assert!(validate_url("http://100.64.0.1/", false).is_some());
    }

    #[test]
    fn rejects_metadata_ip() {
        assert!(validate_url("http://169.254.169.254/", false).is_some());
    }

    #[test]
    fn rejects_ipv4_mapped_metadata() {
        assert!(validate_url("http://[::ffff:169.254.169.254]/", false).is_some());
    }

    #[test]
    fn rejects_localhost_with_dot() {
        assert!(validate_url("http://localhost.:8080/", false).is_some());
    }

    #[test]
    fn allows_public() {
        assert!(validate_url("https://example.com/", false).is_none());
    }

    #[test]
    fn slice_returns_next_offset() {
        let content = "0123456789";
        let (r1, t1, off1) = slice_content(content, 0, 4);
        assert_eq!(r1, "0123");
        assert!(t1);
        assert_eq!(off1, Some(4));
        let (r2, t2, off2) = slice_content(content, 4, 4);
        assert_eq!(r2, "4567");
        assert!(t2);
        assert_eq!(off2, Some(8));
        let (r3, t3, off3) = slice_content(content, 8, 4);
        assert_eq!(r3, "89");
        assert!(!t3);
        assert_eq!(off3, None);
    }

    #[test]
    fn slice_at_exact_boundary_is_not_truncated() {
        let content = "01234567";
        let (part, truncated, next) = slice_content(content, 4, 4);
        assert_eq!(part, "4567");
        assert!(!truncated);
        assert_eq!(next, None);
    }

    #[test]
    fn allows_private_when_flag_set() {
        assert!(validate_url("http://10.0.0.1/", true).is_none());
    }
}
