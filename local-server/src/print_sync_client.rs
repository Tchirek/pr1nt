use reqwest::{Client, RequestBuilder};
use std::time::Duration;
use tracing::warn;

#[derive(Clone)]
pub(crate) struct PrintSyncClient {
    client: Client,
    base_url: String,
    secret: String,
    device_id: String,
}

impl PrintSyncClient {
    pub(crate) fn new(base_url: String, secret: String, device_id: String) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|error| {
                warn!("failed to configure print sync HTTP client: {error}");
                Client::new()
            });

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            secret,
            device_id,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    pub(crate) fn get(&self, path: &str) -> RequestBuilder {
        self.client
            .get(self.url(path))
            .header("x-print-sync-secret", &self.secret)
            .header("x-device-id", &self.device_id)
    }

    pub(crate) fn post(&self, path: &str) -> RequestBuilder {
        self.client
            .post(self.url(path))
            .header("x-print-sync-secret", &self.secret)
            .header("x-device-id", &self.device_id)
    }
}
