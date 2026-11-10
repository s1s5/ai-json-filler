use std::sync::Arc;

use ai_json_filler_core::Tool;

use crate::{FetchTool, SearchTool};

pub struct WebTools {
    tools: Vec<Arc<dyn Tool>>,
}

impl WebTools {
    pub fn builder() -> WebToolsBuilder {
        WebToolsBuilder::default()
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> WebTools {
        WebTools::builder()
            .tavily_key_opt(lookup("TAVILY_API_KEY"))
            .searxng_url_opt(lookup("SEARXNG_URL"))
            .crawl4ai_url_opt(lookup("CRAWL4AI_URL"))
            .build()
    }

    pub fn from_env() -> WebTools {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn into_tools(self) -> Vec<Arc<dyn Tool>> {
        self.tools
    }
}

/// Builds the web tool set.
///
/// By default one capability maps to one tool (`web_search` / `web_fetch`).
/// Provider-suffixed names (`web_search_tavily`, ...) are only registered when
/// [`WebToolsBuilder::provider_aliases`] is enabled.
#[derive(Default)]
pub struct WebToolsBuilder {
    tavily_api_key: Option<String>,
    searxng_url: Option<String>,
    crawl4ai_url: Option<String>,
    provider_aliases: bool,
}

impl WebToolsBuilder {
    pub fn tavily_key(mut self, key: impl Into<String>) -> Self {
        self.tavily_api_key = Some(key.into());
        self
    }

    pub fn searxng_url(mut self, url: impl Into<String>) -> Self {
        self.searxng_url = Some(url.into());
        self
    }

    pub fn crawl4ai_url(mut self, url: impl Into<String>) -> Self {
        self.crawl4ai_url = Some(url.into());
        self
    }

    pub fn provider_aliases(mut self, yes: bool) -> Self {
        self.provider_aliases = yes;
        self
    }

    pub fn build(self) -> WebTools {
        let tavily_key = self.tavily_api_key.filter(|k| !k.trim().is_empty());
        let searxng_url = self.searxng_url.filter(|u| !u.trim().is_empty());
        let crawl4ai_url = self.crawl4ai_url.filter(|u| !u.trim().is_empty());

        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        match tavily_key.as_deref() {
            Some(key) => {
                tools.push(Arc::new(SearchTool::default_tavily(key)));
                tools.push(Arc::new(FetchTool::default_tavily(key)));
            }
            None => {
                if let Some(url) = searxng_url.as_deref() {
                    tools.push(Arc::new(SearchTool::default_searxng(url)));
                }
                if let Some(url) = crawl4ai_url.as_deref() {
                    tools.push(Arc::new(FetchTool::default_crawl4ai(url)));
                }
            }
        }

        if self.provider_aliases {
            if let Some(key) = tavily_key.as_deref() {
                tools.push(Arc::new(SearchTool::tavily(key)));
                tools.push(Arc::new(FetchTool::tavily(key)));
            }
            if let Some(url) = searxng_url.as_deref() {
                tools.push(Arc::new(SearchTool::searxng(url)));
            }
            if let Some(url) = crawl4ai_url.as_deref() {
                tools.push(Arc::new(FetchTool::crawl4ai(url)));
            }
        }

        WebTools { tools }
    }

    fn tavily_key_opt(mut self, key: Option<String>) -> Self {
        self.tavily_api_key = key;
        self
    }

    fn searxng_url_opt(mut self, url: Option<String>) -> Self {
        self.searxng_url = url;
        self
    }

    fn crawl4ai_url_opt(mut self, url: Option<String>) -> Self {
        self.crawl4ai_url = url;
        self
    }
}
