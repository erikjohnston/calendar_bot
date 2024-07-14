pub mod app;
pub mod auth;
pub mod calendar;
pub mod config;
pub mod database;
pub mod site;

use std::path::Path;

use anyhow::{ensure, Context, Error};
use app::App;
use bb8_postgres::tokio_postgres::NoTls;
use clap::ArgMatches;
use database::Database;
use tera::Tera;
use tokio::task::spawn_local;

use crate::config::Config;

/// Default markdown template used for generating reminder events.
const DEFAULT_TEMPLATE: &str = r#"
**{{ summary }}** {{#if (gt minutes_before 0) }}starts in {{ duration }} {{/if}}{{#if location}}at {{ location }} {{/if}}{{#if attendees}} ─ {{ attendees }}{{/if}}{{#if description}}

**Description:** {{ description }}
{{/if}}
"#;

pub async fn create_database(config: &Config) -> Result<Database, Error> {
    let manager = bb8_postgres::PostgresConnectionManager::new_from_stringlike(
        &config.database.connection_string,
        NoTls,
    )?;
    let db_pool = bb8::Pool::builder().max_size(15).build(manager).await?;

    {
        let conn = db_pool.get().await.context("connecting to postgres")?;
        let row = conn.query_one("SELECT 1", &[]).await?;
        ensure!(row.get::<_, i32>(0) == 1, "Got invalid result from DB");
    }

    Ok(Database::from_pool(db_pool))
}

pub async fn create_user(config: Config, args: &ArgMatches) -> Result<(), Error> {
    let database = create_database(&config).await?;
    let username = args.get_one::<String>("username").unwrap();
    let password = args.get_one::<String>("password").unwrap();
    let user_id = database.upsert_account(username).await?;
    database.change_password(user_id, password).await?;
    Ok(())
}

pub async fn create_app(config: Config) -> Result<App, Error> {
    let database = create_database(&config).await?;

    let resource_directory = Path::new(config.app.resource_directory.as_deref().unwrap_or("res"));

    let templates = Tera::new(&resource_directory.join("*").to_string_lossy())?;

    let app = App::new(config, database, templates).await?;

    Ok(app)
}

pub async fn start(config: Config) -> Result<(), Error> {
    let app = create_app(config).await?;

    spawn_local(app.clone().run());

    site::run_server(app).await?;

    Ok(())
}
