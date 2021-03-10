//! Config file structures.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database: DatabaseConfig,
    pub matrix: MatrixConfig,

    #[serde(default)]
    pub app: AppConfig,
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
pub struct AppConfig {
    pub bind_addr: Option<String>,
    pub resource_directory: Option<String>,
}
