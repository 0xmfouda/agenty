use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};

/// Searches the web via DuckDuckGo HTML and returns the top results.
pub struct WebSearchTool;

#[derive(Deserialize)]
struct Input {
    query: String,
    #[serde(default = "default_count")]
    count: usize,
}

fn default_count() -> usize {
    5
}

/// A single search result scraped from the DuckDuckGo HTML page.
#[derive(serde::Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo and return the top results (title, URL, snippet)."
    }

    fn input_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                },
                "count": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 5)."
                }
            },
            "required": ["query"]
        })
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let Input { query, count } =
            serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;

        let results = scrape_duckduckgo(&query, count)?;

        serde_json::to_value(&results).map_err(|e| format!("serialization error: {e}"))
    }
}

fn scrape_duckduckgo(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoded(query));

    // Run the async reqwest call on a dedicated thread to avoid
    // "cannot drop a runtime inside an async context" panics.
    let body = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("failed to create runtime: {e}"))?;
        rt.block_on(async {
            let client = reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                .build()
                .map_err(|e| format!("failed to build HTTP client: {e}"))?;

            let resp = client.get(&url).send().await
                .map_err(|e| format!("request failed: {e}"))?;

            if !resp.status().is_success() {
                return Err(format!("DuckDuckGo returned HTTP {}", resp.status()));
            }

            resp.text().await
                .map_err(|e| format!("failed to read response body: {e}"))
        })
    })
    .join()
    .map_err(|_| "HTTP request thread panicked".to_string())??;

    let document = scraper::Html::parse_document(&body);

    let result_sel =
        scraper::Selector::parse(".result").map_err(|e| format!("bad selector: {e:?}"))?;
    let title_sel =
        scraper::Selector::parse(".result__a").map_err(|e| format!("bad selector: {e:?}"))?;
    let snippet_sel =
        scraper::Selector::parse(".result__snippet").map_err(|e| format!("bad selector: {e:?}"))?;

    let mut results = Vec::new();

    for element in document.select(&result_sel) {
        if results.len() >= max_results {
            break;
        }

        let title_el = match element.select(&title_sel).next() {
            Some(el) => el,
            None => continue,
        };

        let title = title_el.text().collect::<String>().trim().to_string();

        let url = title_el.value().attr("href").unwrap_or("").to_string();

        // Skip ad/empty results
        if title.is_empty() || url.is_empty() {
            continue;
        }

        let snippet = element
            .select(&snippet_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(SearchResult {
            title,
            url,
            snippet,
        });
    }

    if results.is_empty() {
        return Err("no results found".to_string());
    }

    Ok(results)
}

/// Minimal percent-encoding for query parameters.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(HEX[(b >> 4) as usize]));
                out.push(char::from(HEX[(b & 0x0f) as usize]));
            }
        }
    }
    out
}

const HEX: [u8; 16] = *b"0123456789ABCDEF";
