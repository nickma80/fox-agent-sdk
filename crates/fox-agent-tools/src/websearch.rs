use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput, intent_schema_property};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

/// Web search using DuckDuckGo (HTML scraping) with optional Bing API support.
pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct WebSearchInput {
    query: String,
    #[serde(default)]
    num_results: Option<usize>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    bing_market: Option<String>,
    #[serde(default)]
    #[expect(dead_code)]
    intent: Option<String>,
}

#[derive(Debug)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo or Bing."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "intent": intent_schema_property(),
                "query": {
                    "type": "string",
                    "description": "Search query."
                },
                "num_results": {
                    "type": "integer",
                    "description": "Max results (default 8, max 20)."
                },
                "engine": {
                    "type": "string",
                    "enum": ["duckduckgo", "bing"],
                    "description": "Search engine. Defaults to duckduckgo. Bing uses FOX_BING_API_KEY env var when set."
                },
                "bing_market": {
                    "type": "string",
                    "description": "Optional Bing market, e.g. en-US or zh-CN."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: WebSearchInput =
            serde_json::from_value(input).map_err(|e| ToolError::Message {
                message: format!("invalid websearch input: {e}"),
            })?;
        let num_results = params.num_results.unwrap_or(8).min(20);

        let engine = params.engine.as_deref().unwrap_or("duckduckgo");
        let market = params.bing_market.as_deref().unwrap_or("en-US");

        let results = match engine {
            "duckduckgo" => self.search_duckduckgo(&params.query, num_results).await?,
            "bing" => self.search_bing(&params.query, num_results, market).await?,
            _ => {
                return Err(ToolError::Message {
                    message: format!("Unknown engine: {engine}. Use duckduckgo or bing."),
                });
            }
        };

        if results.is_empty() {
            return Ok(ToolOutput {
                text: format!("No results found for: {}", params.query),
                is_error: false,
                json: None,
            });
        }

        let mut output = format!("Search results for: {}\n\n", params.query);
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. **{}**\n   {}\n   {}\n\n",
                i + 1,
                result.title,
                result.url,
                result.snippet
            ));
        }

        Ok(ToolOutput {
            text: output,
            is_error: false,
            json: Some(json!({
                "query": params.query,
                "engine": engine,
                "result_count": results.len(),
            })),
        })
    }
}

impl WebSearchTool {
    async fn search_duckduckgo(
        &self,
        query: &str,
        num_results: usize,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ToolError::Message {
                message: format!("DuckDuckGo search failed: {e}"),
            })?;

        if !response.status().is_success() {
            return Err(ToolError::Message {
                message: format!(
                    "DuckDuckGo search failed with status: {}",
                    response.status()
                ),
            });
        }

        Ok(parse_ddg_results(
            &response.text().await.map_err(|e| ToolError::Message {
                message: format!("failed to read response: {e}"),
            })?,
            num_results,
        ))
    }

    async fn search_bing(
        &self,
        query: &str,
        num_results: usize,
        market: &str,
    ) -> Result<Vec<SearchResult>, ToolError> {
        // Try Bing API first if API key is available
        if let Ok(api_key) = std::env::var("FOX_BING_API_KEY")
            && !api_key.trim().is_empty()
        {
            return self
                .search_bing_api(query, num_results, market, &api_key)
                .await;
        }

        // Fall back to HTML scraping
        self.search_bing_html(query, num_results, market).await
    }

    async fn search_bing_api(
        &self,
        query: &str,
        num_results: usize,
        market: &str,
        api_key: &str,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let response = self
            .client
            .get("https://api.bing.microsoft.com/v7.0/search")
            .query(&[
                ("q", query),
                ("count", &num_results.to_string()),
                ("mkt", market),
            ])
            .header("Ocp-Apim-Subscription-Key", api_key)
            .send()
            .await
            .map_err(|e| ToolError::Message {
                message: format!("Bing API search failed: {e}"),
            })?;

        if !response.status().is_success() {
            return Err(ToolError::Message {
                message: format!("Bing API search failed with status: {}", response.status()),
            });
        }

        Ok(parse_bing_api_results(
            response.json().await.map_err(|e| ToolError::Message {
                message: format!("failed to parse Bing API response: {e}"),
            })?,
            num_results,
        ))
    }

    async fn search_bing_html(
        &self,
        query: &str,
        num_results: usize,
        market: &str,
    ) -> Result<Vec<SearchResult>, ToolError> {
        let url = format!(
            "https://www.bing.com/search?q={}&mkt={}",
            urlencoding::encode(query),
            urlencoding::encode(market)
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ToolError::Message {
                message: format!("Bing search failed: {e}"),
            })?;

        if !response.status().is_success() {
            return Err(ToolError::Message {
                message: format!("Bing search failed with status: {}", response.status()),
            });
        }

        Ok(parse_bing_html_results(
            &response.text().await.map_err(|e| ToolError::Message {
                message: format!("failed to read response: {e}"),
            })?,
            num_results,
        ))
    }
}

// ── HTML parsing utilities ──

mod search_regex {
    use super::*;

    fn compile_regex(pattern: &str, label: &str) -> Option<Regex> {
        Regex::new(pattern)
            .map_err(|err| {
                eprintln!("websearch: failed to compile regex {label}: {err}");
            })
            .ok()
    }

    macro_rules! static_regex {
        ($name:ident, $pat:expr) => {
            pub fn $name() -> Option<&'static Regex> {
                static RE: OnceLock<Option<Regex>> = OnceLock::new();
                RE.get_or_init(|| compile_regex($pat, stringify!($name)))
                    .as_ref()
            }
        };
    }

    static_regex!(
        result_link,
        r#"<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>([^<]*)</a>"#
    );
    static_regex!(
        result_snippet,
        r#"<a[^>]*class="result__snippet"[^>]*>([^<]*(?:<[^>]*>[^<]*)*)</a>"#
    );
    static_regex!(tag, r"<[^>]+>");
    static_regex!(
        bing_result_block,
        r#"(?s)<li[^>]*class="[^"]*\bb_algo\b[^"]*"[^>]*>(.*?)</li>"#
    );
    static_regex!(
        bing_link,
        r#"(?s)<h2[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>\s*</h2>"#
    );
    static_regex!(
        bing_caption,
        r#"(?s)<div[^>]*class="[^"]*\bb_caption\b[^"]*"[^>]*>.*?<p[^>]*>(.*?)</p>"#
    );
}

#[derive(Deserialize)]
struct BingApiResponse {
    #[serde(rename = "webPages")]
    web_pages: Option<BingWebPages>,
}

#[derive(Deserialize)]
struct BingWebPages {
    value: Vec<BingWebPage>,
}

#[derive(Deserialize)]
struct BingWebPage {
    name: String,
    url: String,
    #[serde(default)]
    snippet: String,
}

fn parse_bing_api_results(response: BingApiResponse, max_results: usize) -> Vec<SearchResult> {
    response
        .web_pages
        .map(|pages| {
            pages
                .value
                .into_iter()
                .take(max_results)
                .map(|page| SearchResult {
                    title: page.name,
                    url: page.url,
                    snippet: page.snippet,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_bing_html_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let (Some(block_re), Some(link_re), Some(caption_re), Some(tag_re)) = (
        search_regex::bing_result_block(),
        search_regex::bing_link(),
        search_regex::bing_caption(),
        search_regex::tag(),
    ) else {
        return results;
    };

    for block in block_re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let Some(link) = link_re.captures(&block[1]) else {
            continue;
        };
        let url = html_decode(&link[1]);
        if !url.starts_with("http") || url.contains("bing.com") {
            continue;
        }
        let title = html_decode(&tag_re.replace_all(&link[2], ""));
        let snippet = caption_re
            .captures(&block[1])
            .map(|cap| html_decode(&tag_re.replace_all(&cap[1], "")))
            .unwrap_or_default();
        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

fn parse_ddg_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    let (Some(result_link), Some(result_snippet), Some(tag)) = (
        search_regex::result_link(),
        search_regex::result_snippet(),
        search_regex::tag(),
    ) else {
        return results;
    };

    let links: Vec<_> = result_link.captures_iter(html).collect();
    let snippets: Vec<_> = result_snippet.captures_iter(html).collect();

    for (i, link_cap) in links.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }

        let url = decode_ddg_url(&link_cap[1]);
        let title = html_decode(&link_cap[2]);

        if !url.starts_with("http") || url.contains("duckduckgo.com") {
            continue;
        }

        let snippet = if i < snippets.len() {
            let raw = &snippets[i][1];
            html_decode(&tag.replace_all(raw, ""))
        } else {
            String::new()
        };

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    results
}

fn decode_ddg_url(url: &str) -> String {
    if let Some(uddg_start) = url.find("uddg=") {
        let start = uddg_start + 5;
        let end = url[start..]
            .find('&')
            .map(|i| start + i)
            .unwrap_or(url.len());
        let encoded = &url[start..end];
        urlencoding::decode(encoded)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| encoded.to_string())
    } else {
        url.to_string()
    }
}

fn html_decode(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_html_results() {
        let html = r#"
            <li class="b_algo">
              <h2><a href="https://example.com/rust">Rust &amp; Cargo</a></h2>
              <div class="b_caption"><p>A <strong>systems</strong> language.</p></div>
            </li>
            <li class="b_algo"><h2><a href="https://www.bing.com/aclk">ad</a></h2></li>
            <li class="b_algo">
              <h2><a href="https://example.org/jcode">Jcode</a></h2>
              <div class="b_caption"><p>Agentic coding.</p></div>
            </li>
        "#;

        let results = parse_bing_html_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust & Cargo");
        assert_eq!(results[0].url, "https://example.com/rust");
        assert_eq!(results[0].snippet, "A systems language.");
        assert_eq!(results[1].title, "Jcode");
    }

    #[test]
    fn parses_bing_api_results() {
        let response: BingApiResponse = serde_json::from_value(json!({
            "webPages": {
                "value": [
                    {"name": "One", "url": "https://one.test", "snippet": "first"},
                    {"name": "Two", "url": "https://two.test", "snippet": "second"}
                ]
            }
        }))
        .unwrap();

        let results = parse_bing_api_results(response, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "One");
        assert_eq!(results[0].url, "https://one.test");
    }
}
