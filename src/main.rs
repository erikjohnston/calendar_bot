use std::{
    collections::{BTreeMap, VecDeque},
    convert::TryInto,
    error::Error as StdError,
    fs,
    ops::Deref,
    str::FromStr,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, bail, Context, Error};
use bb8_postgres::tokio_postgres::NoTls;
use chrono::{DateTime, Duration, Utc};
use clap::{crate_authors, crate_description, crate_name, crate_version, value_t_or_exit, Arg};
use comrak::{markdown_to_html, ComrakOptions};
use config::Config;
use handlebars::Handlebars;
use ics_parser::components::VCalendar;
use ics_parser::parser;
use itertools::Itertools;

use reqwest::Method;
use serde_json::json;
use tokio::{
    sync::Notify,
    time::{interval, sleep},
};
use tracing::{error, info, instrument, Span};

mod config;
mod database;

use database::{Attendee, Database, Event, EventInstance, Reminder};

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

/// Parse a ICS encoded calendar.
fn decode_calendar(cal_body: &str) -> Result<Vec<VCalendar>, Error> {
    let components =
        parser::Component::from_str_to_stream(&cal_body).with_context(|| "decoding component")?;

    components
        .into_iter()
        .map(|comp| comp.try_into().with_context(|| "decoding VCALENDAR"))
        .collect()
}

/// Fetch a calendar from a CalDAV URL and parse the returned set of calendars.
///
/// Note that CalDAV returns a calendar per event, rather than one calendar with
/// many events.
#[instrument(skip(client, password), fields(status))]
async fn get_events_for_calendar(
    client: &reqwest::Client,
    url: &str,
    user_name: Option<&str>,
    password: Option<&str>,
) -> Result<Vec<VCalendar>, Error> {
    let mut req = client
        .request(Method::from_str("REPORT").expect("method"), url)
        .header("Content-Type", "application/xml");

    if let Some(user) = user_name {
        req = req.basic_auth(user, password);
    }

    let resp = req
        .body(format!(
            r#"
        <c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
            <d:prop>
                <d:getetag />
                <c:calendar-data />
            </d:prop>
            <c:filter>
                <c:comp-filter name="VCALENDAR">
                    <c:comp-filter name="VEVENT" >
                    <c:time-range start="{start}" />
                    </c:comp-filter>
                </c:comp-filter>
            </c:filter>
        </c:calendar-query>
        "#,
            start = Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
        .send()
        .await?;

    let status = resp.status();

    let body = resp.text().await?;

    info!(status = status.as_u16(), "Got result from CalDAV");
    Span::current().record("status", &status.as_u16());

    if !status.is_success() {
        bail!("Got {} result from CalDAV", status.as_u16());
    }

    let doc = roxmltree::Document::parse(&body)
        .map_err(|e| anyhow!(e))
        .with_context(|| "decoding xml")?;

    let mut calendars = Vec::new();

    for node in doc.descendants() {
        if node.tag_name().name() != "calendar-data" {
            continue;
        }

        let cal_body = if let Some(t) = node.text() {
            t
        } else {
            continue;
        };

        match decode_calendar(cal_body) {
            Ok(cals) => calendars.extend(cals),
            Err(e) => error!(error = e.deref() as &dyn StdError, "Failed to parse event"),
        }
    }

    Ok(calendars)
}

type ReminderInner = Arc<Mutex<VecDeque<(DateTime<Utc>, Reminder)>>>;

#[derive(Debug, Clone, Default)]
struct Reminders {
    inner: ReminderInner,
}

impl Reminders {
    fn get_time_to_next(&self) -> Option<Duration> {
        let inner = self.inner.lock().expect("poisoned");

        inner.front().map(|(t, _)| *t - Utc::now())
    }

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

    fn update(&self, reminders: VecDeque<(DateTime<Utc>, Reminder)>) {
        let mut inner = self.inner.lock().expect("poisoned");

        *inner = reminders;
    }
}

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
            let calendars = get_events_for_calendar(
                &self.http_client,
                &db_calendar.url,
                db_calendar.user_name.as_deref(),
                db_calendar.password.as_deref(),
            )
            .await?;

            let calendar_id = db_calendar.calendar_id;

            let now = Utc::now();

            let mut events = Vec::new();
            let mut next_dates = Vec::new();
            for calendar in &calendars {
                for (uid, event) in &calendar.events {
                    if event.base_event.is_full_day_event() {
                        continue;
                    }

                    events.push(Event {
                        event_id: uid,
                        summary: event.base_event.summary.as_deref(),
                        description: event.base_event.description.as_deref(),
                        location: event.base_event.location.as_deref(),
                    });

                    for (date, recur_event) in event
                        .recur_iter(&calendar)?
                        .skip_while(|(d, _)| *d < now)
                        .take_while(|(d, _)| *d < now + Duration::days(30))
                    {
                        let mut attendees = Vec::new();
                        'prop_loop: for prop in &recur_event.properties {
                            if let ics_parser::property::Property::Attendee(prop) = prop {
                                if prop.value.scheme() != "mailto" {
                                    continue;
                                }

                                let email = prop.value.path().to_string();

                                let mut common_name = None;
                                for param in prop.parameters.parameters() {
                                    match param {
                                        ics_parser::parameters::Parameter::CN(cn) => {
                                            common_name = Some(cn.clone());
                                        }
                                        ics_parser::parameters::Parameter::ParticipationStatus(
                                            status,
                                        ) if status == "DECLINED" => {
                                            continue 'prop_loop;
                                        }
                                        _ => {}
                                    }
                                }

                                attendees.push(Attendee { email, common_name })
                            }
                        }

                        next_dates.push(EventInstance {
                            event_id: uid,
                            date,
                            attendees,
                        });
                    }
                }
            }

            self.database
                .insert_events(calendar_id, events, next_dates)
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

        self.reminders.update(reminders);
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
