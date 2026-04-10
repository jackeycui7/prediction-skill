/// HTTP client wrapper for the Predict WorkNet Coordinator API.

use anyhow::{bail, Context, Result};
use reqwest::{blocking::Client, StatusCode};
use serde_json::Value;

use crate::auth::{build_auth_headers, get_address};

pub struct ApiClient {
    pub base_url: String,
    pub address: String,
    client: Client,
}

impl ApiClient {
    pub fn new(base_url: String) -> Result<Self> {
        let address = get_address()
            .context("Could not determine wallet address. Set AWP_ADDRESS, AWP_PRIVATE_KEY, or configure awp-wallet.")?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { base_url, address, client })
    }

    /// GET an unauthenticated endpoint.
    pub fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url).send().context("Request failed")?;
        self.parse_response(resp)
    }

    /// GET an authenticated endpoint.
    pub fn get_auth(&self, path: &str) -> Result<Value> {
        let auth = build_auth_headers(&self.address)?;
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client
            .get(&url)
            .header("X-AWP-Address", &auth.address)
            .header("X-AWP-Timestamp", &auth.timestamp)
            .header("X-AWP-Signature", &auth.signature)
            .send()
            .context("Request failed")?;
        self.parse_response(resp)
    }

    /// POST an authenticated endpoint with a JSON body.
    pub fn post_auth(&self, path: &str, body: &Value) -> Result<Value> {
        let auth = build_auth_headers(&self.address)?;
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client
            .post(&url)
            .header("X-AWP-Address", &auth.address)
            .header("X-AWP-Timestamp", &auth.timestamp)
            .header("X-AWP-Signature", &auth.signature)
            .json(body)
            .send()
            .context("Request failed")?;
        self.parse_response(resp)
    }

    fn parse_response(&self, resp: reqwest::blocking::Response) -> Result<Value> {
        let status = resp.status();
        let body: Value = resp.json().context("Response was not valid JSON")?;

        if status == StatusCode::OK || status == StatusCode::CREATED {
            Ok(body)
        } else {
            // Server returns { "error": { "code": "...", "message": "...", ... } }
            bail!(
                "HTTP {}: {}",
                status,
                serde_json::to_string(&body).unwrap_or_default()
            )
        }
    }
}

/// Try to reach the server health endpoint. Returns Err if unreachable.
pub fn check_server(base_url: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let url = format!("{}/api/v1/feed/stats", base_url);
    let resp = client.get(&url).send().context("Cannot reach coordinator")?;
    if resp.status().is_success() {
        Ok(())
    } else {
        bail!("Coordinator returned HTTP {}", resp.status())
    }
}
