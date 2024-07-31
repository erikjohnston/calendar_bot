//! # Calendar Bot
//!
//! Calendar Bot is an app that connects to online calendars (via CalDAV) and
//! allows scheduling reminders for them, which are sent to Matrix rooms.
//! Updates to events are correctly handled by the associated reminders.

use std::fs;

use anyhow::{Context, Error};
use calendar_bot::config::Config;
use clap::{Arg, Command};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Entry point.
///
/// We can't just use `tokio::main` here as sentry needs to be setup before
/// spawning threads.
fn main() -> Result<(), Error> {
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

    // Run the async main function on the runtime
    actix_web::rt::System::new().block_on(async_main(matches, config))
}

/// Async entry point
async fn async_main(matches: clap::ArgMatches, config: Config) -> Result<(), Error> {
    match matches.subcommand() {
        Some(("create-user", submatches)) => calendar_bot::create_user(config, submatches).await,
        _ => calendar_bot::start(config).await,
    }
}
