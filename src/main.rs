//! # Calendar Bot
//!
//! Calendar Bot is an app that connects to online calendars (via CalDAV) and
//! allows scheduling reminders for them, which are sent to Matrix rooms.
//! Updates to events are correctly handled by the associated reminders.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error as StdError,
    fs,
    ops::Deref,
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context, Error};
use bb8_postgres::tokio_postgres::NoTls;
use calendar::{fetch_calendars, parse_calendars_to_events};
use chrono::{DateTime, Duration, Utc};
use clap::{crate_authors, crate_description, crate_name, crate_version, value_t_or_exit, Arg};
use comrak::{markdown_to_html, ComrakOptions};
use config::Config;
use handlebars::Handlebars;

use itertools::Itertools;

use serde_json::json;
use tokio::{
    sync::Notify,
    time::{interval, sleep},
};
use tracing::{error, info, instrument, Span};

mod calendar;
mod config;
mod database;

use database::{Database, Reminder};

/// Default markdown template used for generating reminder events.
const DEFAULT_TEMPLATE: &str = r#"
#### {{ summary }}
{{#if (gt minutes_before 0) }}Starts in {{ minutes_before }} minutes{{/if}}

{{#if location}}
**Location:** {{ location }}
{{/if}}

{{#if description}}
**Description:** {{ description }}
{{/if}}

{{#if attendees}}
**Attendees:** {{ attendees }}
{{/if}}
"#;

/// Inner type for [`Reminders`]
type ReminderInner = Arc<Mutex<VecDeque<(DateTime<Utc>, Reminder)>>>;

/// The set of reminders that need to be sent out.
#[derive(Debug, Clone, Default)]
struct Reminders {
    inner: ReminderInner,
}

impl Reminders {
    /// Get how long until the next reminder needs to be sent.
    fn get_time_to_next(&self) -> Option<Duration> {
        let inner = self.inner.lock().expect("poisoned");

        inner.front().map(|(t, _)| *t - Utc::now())
    }

    /// Pop all reminders that are ready to be sent now.
    fn pop_due_reminders(&self) -> Vec<Reminder> {
        let mut reminders = self.inner.lock().expect("poisoned");

        let mut due_reminders = Vec::new();
        let now = Utc::now();

        while let Some((date, reminder)) = reminders.pop_front() {
            if date <= now {
                due_reminders.push(reminder);
            } else {
                reminders.push_front((date, reminder));
                break;
            }
        }

        due_reminders
    }

    /// Replace the current set of reminders
    fn replace(&self, reminders: VecDeque<(DateTime<Utc>, Reminder)>) {
        let mut inner = self.inner.lock().expect("poisoned");

        *inner = reminders;
    }
}

/// The high level app.
#[derive(Debug)]
struct AppState {
    config: Config,
    http_client: reqwest::Client,
    database: Database,
    notify_db_update: Notify,
    reminders: Reminders,
    email_to_matrix_id: Arc<Mutex<BTreeMap<String, String>>>,
}

impl AppState {
    /// Fetches and stores updates for the stored calendars.
    #[instrument(skip(self))]
    async fn update_calendars(&self) -> Result<(), Error> {
        let db_calendars = self.database.get_calendars().await?;

        for db_calendar in db_calendars {
            let calendars = fetch_calendars(
                &self.http_client,
                &db_calendar.url,
                db_calendar.user_name.as_deref(),
                db_calendar.password.as_deref(),
            )
            .await?;

            let (events, next_dates) = parse_calendars_to_events(&calendars)?;
            self.database
                .insert_events(db_calendar.calendar_id, events, next_dates)
                .await?;
        }

        self.update_reminders().await?;

        Ok(())
    }

    /// Queries the DB and updates the reminders
    #[instrument(skip(self))]
    async fn update_reminders(&self) -> Result<(), Error> {
        let reminders = self.database.get_next_reminders().await?;

        info!(num = reminders.len(), "Updated reminders");

        self.reminders.replace(reminders);
        self.notify_db_update.notify_waiters();

        Ok(())
    }

    /// Update the email to matrix ID mapping cache.
    #[instrument(skip(self))]
    async fn update_mappings(&self) -> Result<(), Error> {
        let mapping = self.database.get_user_mappings().await?;

        *self.email_to_matrix_id.lock().expect("poisoned") = mapping;

        Ok(())
    }

    async fn update_calendar_loop(&self) {
        let mut interval = interval(Duration::minutes(5).to_std().expect("std duration"));

        loop {
            interval.tick().await;

            if let Err(error) = self.update_calendars().await {
                error!(
                    error = error.deref() as &dyn StdError,
                    "Failed to update calendars"
                );
            }
        }
    }

    async fn update_mappings_loop(&self) {
        let mut interval = interval(Duration::minutes(5).to_std().expect("std duration"));

        loop {
            interval.tick().await;

            if let Err(error) = self.update_mappings().await {
                error!(
                    error = error.deref() as &dyn StdError,
                    "Failed to update mappings"
                );
            }
        }
    }

    async fn reminder_loop(&self) {
        loop {
            let next_wakeup = self
                .reminders
                .get_time_to_next()
                .unwrap_or_else(|| Duration::minutes(1));

            info!(
                time_to_next = ?self.reminders.get_time_to_next(),
                "Next reminder"
            );

            // `to_std` will fail if the duration is negative, but if that is
            // the case then we have due reminders that we can process
            // immediately.
            if let Ok(dur) = next_wakeup.to_std() {
                let sleep_fut = sleep(dur);
                tokio::pin!(sleep_fut);

                tokio::select! {
                    _ = sleep_fut => {},
                    _ = self.notify_db_update.notified() => {},
                }
            }

            for reminder in self.reminders.pop_due_reminders() {
                if let Err(err) = self.send_reminder(reminder).await {
                    error!(
                        error = err.deref() as &dyn StdError,
                        "Failed to send reminder"
                    );
                }
            }
        }
    }

    /// Send the reminder to the appropriate room.
    #[instrument(skip(self), fields(status))]
    async fn send_reminder(&self, reminder: Reminder) -> Result<(), Error> {
        let join_url = format!(
            "{}/_matrix/client/r0/join/{}",
            self.config.matrix.homeserver_url, reminder.room_id
        );

        let resp = self
            .http_client
            .post(&join_url)
            .bearer_auth(&self.config.matrix.access_token)
            .json(&json!({}))
            .send()
            .await
            .with_context(|| "Sending HTTP /join request")?;

        if !resp.status().is_success() {
            bail!("Got non-2xx from /join response: {}", resp.status());
        }

        let markdown_template = reminder.template.as_deref().unwrap_or(DEFAULT_TEMPLATE);

        let attendees = reminder
            .attendees
            .iter()
            .map(|attendee| {
                if let Some(matrix_id) = self
                    .email_to_matrix_id
                    .lock()
                    .expect("poisoned")
                    .get(&attendee.email)
                {
                    format!(
                        "[{}](https://matrix.to/#/{})",
                        attendee.common_name.as_ref().unwrap_or(matrix_id),
                        matrix_id,
                    )
                } else {
                    attendee
                        .common_name
                        .as_ref()
                        .unwrap_or(&attendee.email)
                        .to_string()
                }
            })
            .join(", ");

        let handlebars = Handlebars::new();
        let markdown = handlebars
            .render_template(
                markdown_template,
                &json!({
                    "event_id": &reminder.event_id,
                    "summary": &reminder.summary,
                    "description": &reminder.description,
                    "location": &reminder.location,
                    "minutes_before": &reminder.minutes_before,
                    "attendees": attendees,
                }),
            )
            .with_context(|| "Rendering body template")?;

        let event_json = json!({
            "msgtype": "m.text",
            "body": markdown,
            "format": "org.matrix.custom.html",
            "formatted_body": markdown_to_html(&markdown, &ComrakOptions::default()),
        });

        let url = format!(
            "{}/_matrix/client/r0/rooms/{}/send/m.room.message",
            self.config.matrix.homeserver_url, reminder.room_id
        );

        let resp = self
            .http_client
            .post(&url)
            .bearer_auth(&self.config.matrix.access_token)
            .json(&event_json)
            .send()
            .await
            .with_context(|| "Sending HTTP send message request")?;

        Span::current().record("status", &resp.status().as_u16());

        info!(
            status = resp.status().as_u16(),
            event_id = reminder.event_id.deref(),
            room_id = reminder.room_id.deref(),
            "Sent reminder"
        );

        if !resp.status().is_success() {
            bail!("Got non-2xx from /send response: {}", resp.status());
        }

        Ok(())
    }
}

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
                .required(true),
        )
        .get_matches();

    let config_file = value_t_or_exit!(matches, "config", String);

    let config: Config =
        toml::from_slice(&fs::read(&config_file).with_context(|| "Reading config file")?)
            .with_context(|| "Parsing config file")?;

    let http_client = reqwest::Client::new();

    let manager = bb8_postgres::PostgresConnectionManager::new_from_stringlike(
        &config.database.connection_string,
        NoTls,
    )?;
    let db_pool = bb8::Pool::builder().max_size(15).build(manager).await?;
    let database = Database::from_pool(db_pool);

    let notify_db_update = Notify::new();
    let state = AppState {
        config,
        http_client,
        database,
        notify_db_update,
        reminders: Default::default(),
        email_to_matrix_id: Default::default(),
    };

    tokio::join!(
        state.update_calendar_loop(),
        state.reminder_loop(),
        state.update_mappings_loop(),
    );

    Ok(())
}
