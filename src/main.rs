//! # Calendar Bot
//!
//! Calendar Bot is an app that connects to online calendars (via CalDAV) and
//! allows scheduling reminders for them, which are sent to Matrix rooms.
//! Updates to events are correctly handled by the associated reminders.

use std::{fs, path::Path};

use anyhow::{Context, Error};
use bb8_postgres::tokio_postgres::NoTls;

use clap::{crate_authors, crate_description, crate_name, crate_version, value_t_or_exit, Arg};

use config::Config;

mod app;
mod auth;
mod calendar;
mod config;
mod database;
mod site;

use app::App;
use database::Database;
use tera::Tera;
use tokio::task::spawn_local;

/// Default markdown template used for generating reminder events.
const DEFAULT_TEMPLATE: &str = r#"
**{{ summary }}** {{#if (gt minutes_before 0) }}starts in {{ minutes_before }} minutes {{/if}}{{#if location}}at {{ location }} {{/if}}{{#if attendees}} ─ {{ attendees }}{{/if}}{{#if description}}

**Description:** {{ description }}
{{/if}}
"#;

/// Entry point.
#[actix_web::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();

    let matches = clap::app_from_crate!()
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("FILE")
                .help("The path to the config file")
                .takes_value(true)
                .default_value("config.toml"),
        )
        .get_matches();

    let config_file = value_t_or_exit!(matches, "config", String);

    let config: Config =
        toml::from_slice(&fs::read(&config_file).with_context(|| "Reading config file")?)
            .with_context(|| "Parsing config file")?;

    let manager = bb8_postgres::PostgresConnectionManager::new_from_stringlike(
        &config.database.connection_string,
        NoTls,
    )?;
    let db_pool = bb8::Pool::builder().max_size(15).build(manager).await?;
    let database = Database::from_pool(db_pool);

    let resource_directory = Path::new(config.app.resource_directory.as_deref().unwrap_or("res"));

    let templates = Tera::new(&resource_directory.join("*").to_string_lossy())?;

    let app = App::new(config, database, templates).await?;

    spawn_local(app.clone().run());

    site::run_server(app).await?;

    Ok(())
}
