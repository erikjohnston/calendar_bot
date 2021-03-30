//! The high level app.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error as StdError,
    ops::Deref,
    sync::{Arc, Mutex},
};

use crate::{
    calendar::{fetch_calendars, parse_calendars_to_events},
    database::ReminderInstance,
};
use crate::{config::Config, database::Database};
use crate::{database::Calendar, DEFAULT_TEMPLATE};

use anyhow::{bail, Context, Error};

use chrono::{DateTime, Duration, Utc};

use comrak::{markdown_to_html, ComrakOptions};
use handlebars::Handlebars;

use itertools::Itertools;

use serde_json::json;
use tera::Tera;
use tokio::{
    sync::Notify,
    time::{interval, sleep},
};
use tracing::{error, info, instrument, Span};

/// Inner type for [`Reminders`]
type ReminderInner = Arc<Mutex<VecDeque<(DateTime<Utc>, ReminderInstance)>>>;

/// The set of reminders that need to be sent out.
#[derive(Debug, Clone, Default)]
pub struct Reminders {
    inner: ReminderInner,
}

impl Reminders {
    /// Get how long until the next reminder needs to be sent.
    fn get_time_to_next(&self) -> Option<Duration> {
        let inner = self.inner.lock().expect("poisoned");

        inner.front().map(|(t, _)| *t - Utc::now())
    }

    /// Pop all reminders that are ready to be sent now.
    fn pop_due_reminders(&self) -> Vec<ReminderInstance> {
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
    fn replace(&self, reminders: VecDeque<(DateTime<Utc>, ReminderInstance)>) {
        let mut inner = self.inner.lock().expect("poisoned");

        *inner = reminders;
    }
}

/// The high level app.
#[derive(Debug, Clone)]
pub struct App {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub database: Database,
    pub notify_db_update: Arc<Notify>,
    pub reminders: Reminders,
    pub email_to_matrix_id: Arc<Mutex<BTreeMap<String, String>>>,
    pub templates: Tera,
}

impl App {
    /// Start the background jobs, including sending reminders and updating calendars.
    pub async fn run(self) {
        tokio::join!(
            self.update_calendar_loop(),
            self.reminder_loop(),
            self.update_mappings_loop(),
        );
    }

    /// Fetches and stores updates for the stored calendars.
    #[instrument(skip(self))]
    pub async fn update_calendars(&self) -> Result<(), Error> {
        let db_calendars = self.database.get_calendars().await?;

        for db_calendar in db_calendars {
            let calendar_id = db_calendar.calendar_id;
            if let Err(error) = self.update_calendar(db_calendar).await {
                error!(
                    error = error.deref() as &dyn StdError,
                    calendar_id, "Failed to update calendar"
                );
            }
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn update_calendar(&self, db_calendar: Calendar) -> Result<(), Error> {
        let calendars = fetch_calendars(
            &self.http_client,
            &db_calendar.url,
            db_calendar.user_name.as_deref(),
            db_calendar.password.as_deref(),
        )
        .await?;

        let (events, next_dates) = parse_calendars_to_events(db_calendar.calendar_id, &calendars)?;

        self.database
            .insert_events(db_calendar.calendar_id, events, next_dates)
            .await?;

        self.update_reminders().await?;

        Ok(())
    }

    /// Queries the DB and updates the reminders
    #[instrument(skip(self))]
    pub async fn update_reminders(&self) -> Result<(), Error> {
        let reminders = self.database.get_next_reminders().await?;

        info!(num = reminders.len(), "Updated reminders");

        self.reminders.replace(reminders);
        self.notify_db_update.notify_one();

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
                .unwrap_or_else(|| Duration::minutes(5))
                .max(Duration::minutes(5));

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
    async fn send_reminder(&self, reminder: ReminderInstance) -> Result<(), Error> {
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
