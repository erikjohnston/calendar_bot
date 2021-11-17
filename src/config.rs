//! Config file structures.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database: DatabaseConfig,
    pub matrix: MatrixConfig,

    pub hibob: Option<HiBobConfig>,

    pub sso: Option<SsoConfig>,

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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HiBobConfig {
    pub token: String,
}

#[derive(Clone, Deserialize, Default)]
pub struct SsoConfig {
    pub display_name: String,
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub base_url: String,
    pub scopes: Vec<String>,
}

// We implement this manually so we can stop `client_secret` from being printed.
impl std::fmt::Debug for SsoConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoConfig")
            .field("display_name", &self.display_name)
            .field("issuer_url", &self.issuer_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.is_some())
            .field("base_url", &self.base_url)
            .field("scopes", &self.scopes)
            .finish()
    }
}
