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
use postgres_types::{FromSql, ToSql};
use reqwest::Method;
use serde_json::json;
use tokio::{
    sync::Notify,
    time::{interval, sleep},
};
use tracing::{error, info, instrument, Span};

mod config;

type PostgresPool = bb8::Pool<bb8_postgres::PostgresConnectionManager<NoTls>>;

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

#[derive(Debug, Clone, ToSql, FromSql)]
struct Attendee {
    email: String,
    common_name: Option<String>,
}

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

#[derive(Debug, Clone)]
struct Reminder {
    event_id: String,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    template: Option<String>,
    minutes_before: i64,
    room_id: String,
    attendees: Vec<Attendee>,
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
    db_pool: PostgresPool,
    notify_db_update: Notify,
    reminders: Reminders,
    email_to_matrix_id: Arc<Mutex<BTreeMap<String, String>>>,
}

impl AppState {
    /// Fetches and stores updates for the stored calendars.
    #[instrument(skip(self))]
    async fn update_calendars(&self) -> Result<(), Error> {
        let mut db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                "SELECT calendar_id, url, user_name, password FROM calendars",
                &[],
            )
            .await?;

        for row in rows {
            let calendar_id: i64 = row.get(0);
            let url: &str = row.get(1);
            let user_name: Option<&str> = row.get(2);
            let password: &str = row.get(3);

            let calendars =
                get_events_for_calendar(&self.http_client, url, user_name, Some(password)).await?;

            let now = Utc::now();

            let mut events = BTreeMap::new();
            let mut event_next_dates = BTreeMap::new();
            for calendar in &calendars {
                for (uid, event) in &calendar.events {
                    if event.base_event.is_full_day_event() {
                        continue;
                    }

                    events.insert(
                        uid,
                        (
                            event.base_event.summary.as_deref(),
                            event.base_event.description.as_deref(),
                            event.base_event.location.as_deref(),
                        ),
                    );

                    let mut next_dates = Vec::new();
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

                        next_dates.push((date, attendees));
                    }

                    event_next_dates.insert(uid, next_dates);
                }
            }

            let txn = db_conn.transaction().await?;

            futures::future::try_join_all(events.iter().map(|(uid, values)| {
                txn.execute_raw(
                    r#"
                INSERT INTO events (calendar_id, event_id, summary, description, location)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (calendar_id, event_id)
                DO UPDATE SET
                    summary = EXCLUDED.summary,
                    description = EXCLUDED.description,
                    location = EXCLUDED.location
            "#,
                    vec![
                        &calendar_id as &dyn ToSql,
                        uid,
                        &values.0,
                        &values.1,
                        &values.2,
                    ],
                )
            }))
            .await?;

            txn.execute(
                "DELETE FROM next_dates WHERE calendar_id = $1",
                &[&calendar_id],
            )
            .await?;

            futures::future::try_join_all(
                event_next_dates
                    .iter()
                    .flat_map(|(uid, values)| {
                        values.iter().map(move |(d, attendees)| (uid, d, attendees))
                    })
                    .map(|(uid, date, attendees)| {
                        txn.execute_raw(
                            r#"
                            INSERT INTO next_dates (calendar_id, event_id, timestamp, attendees)
                            VALUES ($1, $2, $3, $4)
                        "#,
                            vec![&calendar_id as &dyn ToSql, uid, date, attendees],
                        )
                    }),
            )
            .await?;

            txn.commit().await?;
        }

        self.update_reminders().await?;

        Ok(())
    }

    /// Queries the DB and updates the reminders
    #[instrument(skip(self))]
    async fn update_reminders(&self) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT event_id, summary, description, location, timestamp, room_id, minutes_before, template, attendees
                    FROM reminders
                    INNER JOIN events USING (calendar_id, event_id)
                    INNER JOIN next_dates USING (calendar_id, event_id)
                "#,
                &[],
            )
            .await?;

        let mut reminders = VecDeque::with_capacity(rows.len());

        for row in rows {
            let event_id: String = row.get(0);
            let summary: Option<String> = row.get(1);
            let description: Option<String> = row.get(2);
            let location: Option<String> = row.get(3);
            let timestamp: DateTime<Utc> = row.get(4);
            let room_id: String = row.get(5);
            let minutes_before: i64 = row.get(6);
            let template: Option<String> = row.get(7);
            let attendees: Vec<Attendee> = row.get(8);

            let reminder_time = timestamp - Duration::minutes(minutes_before);
            if reminder_time < Utc::now() {
                continue;
            }

            let reminder = Reminder {
                event_id,
                summary,
                description,
                location,
                template,
                minutes_before,
                room_id,
                attendees,
            };

            reminders.push_back((reminder_time, reminder));
        }

        reminders.make_contiguous().sort_by_key(|(t, _)| *t);

        info!(num = reminders.len(), "Updated reminders");

        self.reminders.update(reminders);
        self.notify_db_update.notify_waiters();

        Ok(())
    }

    /// Update the email to matrix ID mapping cache.
    #[instrument(skip(self))]
    async fn update_mappings(&self) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query("SELECT email, matrix_id FROM email_to_matrix_id", &[])
            .await?;

        let mapping: BTreeMap<String, String> = rows
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();

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

#[tokio::main]
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

    let notify_db_update = Notify::new();
    let state = AppState {
        config,
        http_client,
        db_pool,
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
