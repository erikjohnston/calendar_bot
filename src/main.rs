//! # Calendar Bot
//!
//! Calendar Bot is an app that connects to online calendars (via CalDAV) and
//! allows scheduling reminders for them, which are sent to Matrix rooms.
//! Updates to events are correctly handled by the associated reminders.

use std::{fs, path::Path};

use anyhow::{Context, Error};
use bb8_postgres::tokio_postgres::NoTls;

use clap::{Arg, ArgMatches, Command};

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
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default markdown template used for generating reminder events.
const DEFAULT_TEMPLATE: &str = r#"
**{{ summary }}** {{#if (gt minutes_before 0) }}starts in {{ duration }} {{/if}}{{#if location}}at {{ location }} {{/if}}{{#if attendees}} ─ {{ attendees }}{{/if}}{{#if description}}

**Description:** {{ description }}
{{/if}}
"#;

/// Entry point.
#[actix_web::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(sentry_tracing::layer())
        .init();

    let matches = clap::command!()
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("The path to the config file")
                .num_args(1)
                .default_value("config.toml"),
        )
        .subcommand(
            Command::new("create-user")
                .arg(Arg::new("username").required(true))
                .arg(Arg::new("password").required(true)),
        )
        .get_matches();

    let config_file = matches.get_one::<String>("config").unwrap();
    let config_bytes = fs::read(config_file).with_context(|| "Reading config file")?;
    let config_str = String::from_utf8(config_bytes).with_context(|| "Parsing config file")?;

    let config: Config = toml::from_str(&config_str).with_context(|| "Parsing config file")?;

    let _guard = if let Some(sentry_config) = &config.sentry {
        let guard = sentry::init((
            &*sentry_config.dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        ));

        Some(guard)
    } else {
        None
    };

    match matches.subcommand() {
        Some(("create-user", submatches)) => create_user(config, submatches).await,
        _ => start(config).await,
    }
}

async fn create_database(config: &Config) -> Result<Database, Error> {
    let manager = bb8_postgres::PostgresConnectionManager::new_from_stringlike(
        &config.database.connection_string,
        NoTls,
    )?;
    let db_pool = bb8::Pool::builder().max_size(15).build(manager).await?;
    Ok(Database::from_pool(db_pool))
}

async fn create_user(config: Config, args: &ArgMatches) -> Result<(), Error> {
    let database = create_database(&config).await?;
    let username = args.get_one::<String>("username").unwrap();
    let password = args.get_one::<String>("password").unwrap();
    let user_id = database.upsert_account(username).await?;
    database.change_password(user_id, password).await?;
    Ok(())
}

async fn start(config: Config) -> Result<(), Error> {
    let database = create_database(&config).await?;

    let resource_directory = Path::new(config.app.resource_directory.as_deref().unwrap_or("res"));

    let templates = Tera::new(&resource_directory.join("*").to_string_lossy())?;

    let app = App::new(config, database, templates).await?;

    spawn_local(app.clone().run());

    site::run_server(app).await?;

    Ok(())
}
