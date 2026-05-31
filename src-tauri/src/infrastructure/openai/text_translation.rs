use serde::Deserialize;
use serde_json::json;
use std::error::Error as StdError;
use std::time::Duration;

const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const DEFAULT_TEXT_TRANSLATION_MODEL: &str = "gpt-5-mini";

#[derive(Debug, Clone, thiserror::Error)]
pub enum OpenAITextTranslationError {
    #[error("Authentication: {0}")]
    Authentication(String),
    #[error("Rate limited: {0}")]
    RateLimited(String),
    #[error("Connection: {0}")]
    Connection(String),
    #[error("Protocol: {0}")]
    Protocol(String),
}

impl OpenAITextTranslationError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Authentication(_) => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::Connection(_) => "connection",
            Self::Protocol(_) => "processing",
        }
    }
}

#[derive(Clone)]
pub struct OpenAITextTranslationClient {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAITextTranslationClient {
    pub fn new(api_key: String) -> Result<Self, OpenAITextTranslationError> {
        let model = std::env::var("VOICETEXT_INCOMING_TRANSLATION_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_TEXT_TRANSLATION_MODEL.to_string());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|e| OpenAITextTranslationError::Connection(format_reqwest_error(&e)))?;

        Ok(Self {
            api_key,
            model,
            client,
        })
    }

    pub async fn translate_text(
        &self,
        text: &str,
        target_language: &str,
    ) -> Result<String, OpenAITextTranslationError> {
        let input = text.trim();
        if input.is_empty() {
            return Ok(String::new());
        }
        if self.api_key.trim().is_empty() {
            return Err(OpenAITextTranslationError::Authentication(
                "OPENAI_API_KEY не задан".to_string(),
            ));
        }

        let body = json!({
            "model": self.model,
            "instructions": format!(
                "Translate speech transcript into {target_language}. Return only the translation. Preserve meaning, names, numbers, and technical terms. Do not explain."
            ),
            "input": input,
        });

        let response = self
            .client
            .post(OPENAI_RESPONSES_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| OpenAITextTranslationError::Connection(format_reqwest_error(&e)))?;

        let status = response.status();
        let text_body = response
            .text()
            .await
            .map_err(|e| OpenAITextTranslationError::Connection(format_reqwest_error(&e)))?;

        if !status.is_success() {
            let message = extract_openai_error_message(&text_body)
                .unwrap_or_else(|| format!("OpenAI HTTP {}", status.as_u16()));
            return Err(match status.as_u16() {
                401 | 403 => OpenAITextTranslationError::Authentication(message),
                429 => OpenAITextTranslationError::RateLimited(message),
                _ => OpenAITextTranslationError::Connection(message),
            });
        }

        let parsed: ResponsesApiResponse = serde_json::from_str(&text_body).map_err(|e| {
            OpenAITextTranslationError::Protocol(format!("invalid OpenAI response: {}", e))
        })?;

        extract_response_text(parsed).ok_or_else(|| {
            OpenAITextTranslationError::Protocol("OpenAI response has no output text".to_string())
        })
    }
}

#[derive(Debug, Deserialize)]
struct ResponsesApiResponse {
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponseOutputItem>,
}

#[derive(Debug, Deserialize)]
struct ResponseOutputItem {
    #[serde(default)]
    content: Vec<ResponseContentItem>,
}

#[derive(Debug, Deserialize)]
struct ResponseContentItem {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: Option<OpenAIErrorBody>,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorBody {
    message: String,
}

fn extract_response_text(response: ResponsesApiResponse) -> Option<String> {
    if let Some(text) = response.output_text {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let mut parts = Vec::new();
    for item in response.output {
        for content in item.content {
            if let Some(text) = content.text {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn extract_openai_error_message(body: &str) -> Option<String> {
    serde_json::from_str::<OpenAIErrorResponse>(body)
        .ok()
        .and_then(|parsed| parsed.error.map(|err| err.message))
        .filter(|message| !message.trim().is_empty())
}

fn format_reqwest_error(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];

    if err.is_timeout() {
        parts.push("kind=timeout".to_string());
    }
    if err.is_connect() {
        parts.push("kind=connect".to_string());
    }
    if err.is_request() {
        parts.push("kind=request".to_string());
    }
    if err.is_body() {
        parts.push("kind=body".to_string());
    }

    let mut source = err.source();
    while let Some(cause) = source {
        parts.push(format!("cause={}", cause));
        source = cause.source();
    }

    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_output_text_first() {
        let response = ResponsesApiResponse {
            output_text: Some(" Привет ".to_string()),
            output: vec![],
        };

        assert_eq!(extract_response_text(response).as_deref(), Some("Привет"));
    }

    #[test]
    fn extracts_nested_output_text() {
        let response = ResponsesApiResponse {
            output_text: None,
            output: vec![ResponseOutputItem {
                content: vec![ResponseContentItem {
                    text: Some("Здравствуйте".to_string()),
                }],
            }],
        };

        assert_eq!(
            extract_response_text(response).as_deref(),
            Some("Здравствуйте")
        );
    }
}
