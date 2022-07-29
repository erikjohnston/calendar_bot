//! # Calendar Bot
//!
//! Calendar Bot is an app that connects to online calendars (via CalDAV) and
//! allows scheduling reminders for them, which are sent to Matrix rooms.
//! Updates to events are correctly handled by the associated reminders.

use std::{fs, path::Path};

use anyhow::{Context, Error};
use bb8_postgres::tokio_postgres::NoTls;

use clap::{value_t_or_exit, Arg, ArgMatches, SubCommand};

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
**{{ summary }}** {{#if (gt minutes_before 0) }}starts in {{ minutes_before }} minutes {{/if}}{{#if location}}at {{ location }} {{/if}}{{#if attendees}} ─ {{ attendees }}{{/if}}{{#if description}}

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
            Arg::with_name("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("The path to the config file")
                .takes_value(true)
                .default_value("config.toml"),
        )
        .subcommand(
            SubCommand::with_name("create-user")
                .arg(Arg::with_name("username").required(true))
                .arg(Arg::with_name("password").required(true)),
        )
        .get_matches();

    let config_file = value_t_or_exit!(matches, "config", String);

    let config: Config =
        toml::from_slice(&fs::read(&config_file).with_context(|| "Reading config file")?)
            .with_context(|| "Parsing config file")?;

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
    let username = args.value_of("username").unwrap();
    let password = args.value_of("password").unwrap();
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
