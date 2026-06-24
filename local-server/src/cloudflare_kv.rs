use reqwest::{header, Client, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use tracing::warn;

use crate::ApiError;

#[derive(Clone)]
pub(crate) struct CloudflareKvClient {
    client: Client,
    account_id: String,
    namespace_id: String,
    api_token: String,
}

impl CloudflareKvClient {
    pub(crate) fn new(account_id: String, namespace_id: String, api_token: String) -> Self {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .unwrap_or_else(|error| {
                warn!("failed to configure Cloudflare HTTP client timeout: {error}");
                Client::new()
            });

        Self {
            client,
            account_id,
            namespace_id,
            api_token,
        }
    }

    fn value_endpoint(&self, key: &str) -> String {
        format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/storage/kv/namespaces/{}/values/{}",
            self.account_id,
            self.namespace_id,
            urlencoding::encode(key)
        )
    }

    pub(crate) fn namespace_id(&self) -> &str {
        &self.namespace_id
    }

    pub(crate) async fn put_json<T: Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), ApiError> {
        let payload =
            serde_json::to_string(value).map_err(|error| ApiError::internal(error.to_string()))?;
        self.put_text(key, &payload).await
    }

    pub(crate) async fn put_text(&self, key: &str, value: &str) -> Result<(), ApiError> {
        let response = self
            .client
            .put(self.value_endpoint(key))
            .bearer_auth(&self.api_token)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(value.to_owned())
            .send()
            .await
            .map_err(|error| ApiError::upstream(format!("Cloudflare KV write failed: {error}")))?;

        if response.status().is_success() {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown Cloudflare KV error".to_owned());
        Err(ApiError::upstream(format!(
            "Cloudflare KV write failed: {body}"
        )))
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, ApiError> {
        let Some(value) = self.get_text(key).await? else {
            return Ok(None);
        };

        serde_json::from_str(&value).map(Some).map_err(|error| {
            ApiError::upstream(format!(
                "Cloudflare KV JSON decode failed for {key}: {error}"
            ))
        })
    }

    pub(crate) async fn get_text(&self, key: &str) -> Result<Option<String>, ApiError> {
        let response = self
            .client
            .get(self.value_endpoint(key))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|error| ApiError::upstream(format!("Cloudflare KV read failed: {error}")))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown Cloudflare KV error".to_owned());
            return Err(ApiError::upstream(format!(
                "Cloudflare KV read failed: {body}"
            )));
        }

        response
            .text()
            .await
            .map(Some)
            .map_err(|error| ApiError::upstream(format!("Cloudflare KV body read failed: {error}")))
    }

    pub(crate) async fn delete_text(&self, key: &str) -> Result<(), ApiError> {
        let response = self
            .client
            .delete(self.value_endpoint(key))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|error| ApiError::upstream(format!("Cloudflare KV delete failed: {error}")))?;

        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }

        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown Cloudflare KV error".to_owned());
        Err(ApiError::upstream(format!(
            "Cloudflare KV delete failed: {body}"
        )))
    }
}
