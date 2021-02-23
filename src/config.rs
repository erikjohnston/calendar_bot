//! Config file structures.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database: DatabaseConfig,
    pub matrix: MatrixConfig,

    #[serde(default)]
    pub web: WebConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub connection_string: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixConfig {
    pub homeserver_url: String,
    pub access_token: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WebConfig {
    pub bind_addr: Option<String>,
}
