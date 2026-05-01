//! Description enrichment middleware that overrides or appends to tool descriptions.

use async_trait::async_trait;
use rmcp::model::{Prompt, Resource, Tool};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use crate::error::Result;
use crate::middleware::proxy::ProxyMiddleware;
use crate::middleware::ProxyMiddlewareFactory;

/// Middleware that overrides or enriches item descriptions.
#[derive(Debug)]
pub struct DescriptionEnricherMiddleware {
    suffix: String,
    overrides: HashMap<String, String>,
}

impl DescriptionEnricherMiddleware {
    pub fn new(suffix: String, overrides: HashMap<String, String>) -> Self {
        Self { suffix, overrides }
    }
}

#[async_trait]
impl ProxyMiddleware for DescriptionEnricherMiddleware {
    async fn on_list_tools(&self, tools: &mut Vec<Tool>) {
        for tool in tools.iter_mut() {
            let name = tool.name.to_string();
            if let Some(replacement) = self.overrides.get(&name) {
                tool.description = Some(replacement.clone().into());
            } else if let Some(ref mut description) = tool.description {
                if !self.suffix.is_empty() {
                    *description = format!("{}{}", description, self.suffix).into();
                }
            }
        }

        if !tools.is_empty() {
            info!("✨ Enriched descriptions for {} tools", tools.len());
        }
    }

    async fn on_list_prompts(&self, prompts: &mut Vec<Prompt>) {
        for prompt in prompts.iter_mut() {
            let name = prompt.name.clone();
            if let Some(replacement) = self.overrides.get(&name) {
                prompt.description = Some(replacement.clone());
            } else if let Some(ref description) = prompt.description.clone() {
                if !self.suffix.is_empty() {
                    prompt.description = Some(format!("{}{}", description, self.suffix));
                }
            }
        }

        if !prompts.is_empty() {
            info!("✨ Enriched descriptions for {} prompts", prompts.len());
        }
    }

    async fn on_list_resources(&self, resources: &mut Vec<Resource>) {
        for resource in resources.iter_mut() {
            let uri = resource.uri.clone();
            if let Some(replacement) = self.overrides.get(&uri) {
                resource.description = Some(replacement.clone());
            } else if let Some(ref description) = resource.description.clone() {
                if !self.suffix.is_empty() {
                    resource.description = Some(format!("{}{}", description, self.suffix));
                }
            }
        }

        if !resources.is_empty() {
            info!("✨ Enriched descriptions for {} resources", resources.len());
        }
    }
}

/// Factory for creating DescriptionEnricherMiddleware from configuration
#[derive(Debug)]
pub struct DescriptionEnricherFactory;

impl ProxyMiddlewareFactory for DescriptionEnricherFactory {
    fn create(&self, config: &serde_json::Value) -> Result<Arc<dyn ProxyMiddleware>> {
        let suffix = config
            .get("suffix")
            .and_then(|v| v.as_str())
            .unwrap_or(" (via mcproxy)")
            .to_string();

        let overrides = config
            .get("overrides")
            .and_then(|v| v.as_object())
            .map(|map| {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Arc::new(DescriptionEnricherMiddleware::new(suffix, overrides)))
    }

    fn middleware_type(&self) -> &'static str {
        "description_enricher"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Tool;
    use std::collections::HashMap;

    fn make_tool(name: &str, description: Option<&str>) -> Tool {
        Tool {
            name: name.to_string().into(),
            description: description.map(|d| d.to_string().into()),
            input_schema: Arc::new(serde_json::Map::new()),
            annotations: None,
        }
    }

    #[tokio::test]
    async fn test_default_suffix() {
        let mw = DescriptionEnricherMiddleware::new(" (via mcproxy)".into(), HashMap::new());
        let mut tools = vec![make_tool("t1", Some("original"))];
        mw.on_list_tools(&mut tools).await;
        assert_eq!(&*tools[0].description.as_ref().unwrap(), "original (via mcproxy)");
    }

    #[tokio::test]
    async fn test_override_replaces_description() {
        let mut overrides = HashMap::new();
        overrides.insert("server___tool".into(), "Custom description for LLM".into());
        let mw = DescriptionEnricherMiddleware::new(" (via mcproxy)".into(), overrides);

        let mut tools = vec![
            make_tool("server___tool", Some("boring upstream desc")),
            make_tool("other___tool", Some("keep this")),
        ];
        mw.on_list_tools(&mut tools).await;

        assert_eq!(&*tools[0].description.as_ref().unwrap(), "Custom description for LLM");
        assert_eq!(&*tools[1].description.as_ref().unwrap(), "keep this (via mcproxy)");
    }

    #[tokio::test]
    async fn test_empty_suffix_no_change() {
        let mw = DescriptionEnricherMiddleware::new("".into(), HashMap::new());
        let mut tools = vec![make_tool("t1", Some("original"))];
        mw.on_list_tools(&mut tools).await;
        assert_eq!(&*tools[0].description.as_ref().unwrap(), "original");
    }

    #[tokio::test]
    async fn test_factory_from_config() {
        let config = serde_json::json!({
            "suffix": " [proxy]",
            "overrides": {
                "fetch___fetch": "Fetch a URL and return its contents."
            }
        });
        let factory = DescriptionEnricherFactory;
        let mw = factory.create(&config).unwrap();

        let mut tools = vec![
            make_tool("fetch___fetch", Some("upstream")),
            make_tool("other___x", Some("upstream")),
        ];
        mw.on_list_tools(&mut tools).await;

        assert_eq!(&*tools[0].description.as_ref().unwrap(), "Fetch a URL and return its contents.");
        assert_eq!(&*tools[1].description.as_ref().unwrap(), "upstream [proxy]");
    }
} 