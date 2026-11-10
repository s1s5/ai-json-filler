use ai_json_filler_core::{BoxFuture, FillError, Tool, ToolOutput, ToolSpec};
use serde_json::{Value, json};

use crate::http::{build_client, read_json, to_tool_error};

const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";

pub struct SearchTool {
    name: String,
    client: SearchClient,
    max_results: usize,
    timeout_secs: u64,
}

enum SearchClient {
    Tavily { api_key: String },
    Searxng { base_url: String, language: String },
}

impl SearchTool {
    pub fn tavily(api_key: impl Into<String>) -> Self {
        Self::new(
            "web_search_tavily",
            SearchClient::Tavily {
                api_key: api_key.into(),
            },
        )
    }

    pub fn searxng(base_url: impl Into<String>) -> Self {
        Self::new(
            "web_search_searxng",
            SearchClient::Searxng {
                base_url: base_url.into().trim_end_matches('/').to_string(),
                language: "ja".to_string(),
            },
        )
    }

    pub fn default_tavily(api_key: impl Into<String>) -> Self {
        Self {
            name: "web_search".to_string(),
            ..Self::tavily(api_key)
        }
    }

    pub fn default_searxng(base_url: impl Into<String>) -> Self {
        Self {
            name: "web_search".to_string(),
            ..Self::searxng(base_url)
        }
    }

    fn new(name: &str, client: SearchClient) -> Self {
        Self {
            name: name.to_string(),
            client,
            max_results: 5,
            timeout_secs: 30,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_max_results(mut self, n: usize) -> Self {
        self.max_results = n.clamp(1, 10);
        self
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs.max(1);
        self
    }
}

impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "Web を検索して最新情報を得る。株価・PER・決算・ニュースなど、知識だけでは古くなる情報を調べるときに使う。結果はタイトル・URL・抜粋の配列で返る。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "検索クエリ" },
                    "max_results": { "type": "integer", "description": "取得件数（既定5、最大10）" },
                    "page": { "type": "integer", "description": "ページ番号（1始まり）。SearXNG のみ対応" }
                },
                "required": ["query"]
            }),
        }
    }

    fn prompt_hint(&self) -> Option<&str> {
        Some(
            "【検索の方針】\n\
             - 時間で変わる情報は必ずこのツールで調べる\n\
             - 検索は合計5回まで\n\
             - 結果が足りないときは page を増やす（SearXNG のみ）\n\
             - 検索結果に無い数値を作らない",
        )
    }

    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, Result<ToolOutput, FillError>> {
        Box::pin(async move { self.run(args).await })
    }
}

impl SearchTool {
    async fn run(&self, args: Value) -> Result<ToolOutput, FillError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Ok(ToolOutput::error("SEARCH_ERROR: query が空です"));
        }
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(self.max_results)
            .clamp(1, 10);
        // `page` is 1-based; treat 0 and other out-of-range values as page 1.
        let page = args.get("page").and_then(Value::as_u64).unwrap_or(1).max(1);

        match &self.client {
            SearchClient::Tavily { api_key } => {
                if page > 1 {
                    return Ok(ToolOutput::error(
                        "SEARCH_ERROR: Tavily はページングに対応していません（page=1 のみ）",
                    ));
                }
                let body = json!({
                    "query": query,
                    "max_results": max_results,
                    "search_depth": "basic",
                    "include_answer": false,
                    "include_raw_content": false
                });
                let response = build_client(self.timeout_secs)
                    .post(TAVILY_SEARCH_URL)
                    .bearer_auth(api_key)
                    .json(&body)
                    .send()
                    .await;
                match read_json(response).await {
                    Ok(value) => serialize(parse_tavily(&value, query)),
                    Err(message) => Ok(ToolOutput::error(format!("SEARCH_ERROR: {message}"))),
                }
            }
            SearchClient::Searxng { base_url, language } => {
                let url = format!("{base_url}/search");
                let pageno = page.to_string();
                let url = reqwest::Url::parse_with_params(
                    &url,
                    [
                        ("q", query),
                        ("format", "json"),
                        ("language", language.as_str()),
                        ("pageno", pageno.as_str()),
                    ],
                );
                let url = match url {
                    Ok(url) => url,
                    Err(e) => {
                        return Ok(ToolOutput::error(format!("SEARCH_ERROR: {e}")));
                    }
                };
                let response = build_client(self.timeout_secs).get(url).send().await;
                match read_json(response).await {
                    Ok(value) => serialize(parse_searxng(&value, query, Some(max_results))),
                    Err(message) => Ok(ToolOutput::error(format!("SEARCH_ERROR: {message}"))),
                }
            }
        }
    }
}

/// Normalize a Tavily response into the canonical result shape.
pub(crate) fn parse_tavily(value: &Value, query: &str) -> Value {
    let out: Vec<Value> = results_of(value).filter_map(normalize).collect();
    json!({ "provider": "tavily", "query": query, "results": out })
}

/// Normalize a SearXNG response into the canonical result shape.
pub(crate) fn parse_searxng(value: &Value, query: &str, max_results: Option<usize>) -> Value {
    let out: Vec<Value> = match max_results {
        Some(n) => results_of(value).take(n).filter_map(normalize).collect(),
        None => results_of(value).filter_map(normalize).collect(),
    };
    json!({ "provider": "searxng", "query": query, "results": out })
}

fn results_of(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn normalize(r: &Value) -> Option<Value> {
    let url = r.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        return None;
    }
    Some(json!({
        "title": r.get("title").and_then(Value::as_str).unwrap_or(""),
        "url": url,
        "snippet": r.get("content").and_then(Value::as_str).unwrap_or(""),
    }))
}

fn serialize(value: Value) -> Result<ToolOutput, FillError> {
    match serde_json::to_string(&value) {
        Ok(s) => Ok(ToolOutput::success(s)),
        Err(e) => Err(to_tool_error("web_search", format!("SEARCH_ERROR: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tavily_normalizes() {
        let value = json!({ "results": [{"title": "T", "url": "U", "content": "C"}] });
        let out = parse_tavily(&value, "q");
        assert_eq!(out["provider"], "tavily");
        assert_eq!(out["results"][0]["snippet"], "C");
    }

    #[test]
    fn searxng_normalizes() {
        let value = json!({ "results": [{"title": "T", "url": "U", "content": "C"}] });
        let out = parse_searxng(&value, "q", None);
        assert_eq!(out["provider"], "searxng");
    }

    #[test]
    fn searxng_respects_max_results() {
        let value = json!({
            "results": [
                {"title": "T1", "url": "U1", "content": "C1"},
                {"title": "T2", "url": "U2", "content": "C2"},
                {"title": "T3", "url": "U3", "content": "C3"}
            ]
        });
        let out = parse_searxng(&value, "q", Some(2));
        assert_eq!(out["results"].as_array().unwrap().len(), 2);
    }
}
