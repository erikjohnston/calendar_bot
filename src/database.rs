//! Module for talking to the database

use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
};

use anyhow::Error;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use postgres_types::{FromSql, ToSql};
use tokio_postgres::NoTls;

/// Async database pool for PostgreSQL.
pub type PostgresPool = bb8::Pool<bb8_postgres::PostgresConnectionManager<NoTls>>;

/// An attendee of the meeting.
///
/// Includes people who haven't responded, or are tentative/confirmed.
#[derive(Debug, Clone, ToSql, FromSql)]
pub struct Attendee {
    pub email: String,
    pub common_name: Option<String>,
}

/// The URL and credentials of a calendar.
#[derive(Debug, Clone)]
pub struct Calendar {
    pub calendar_id: i64,
    pub url: String,
    pub user_name: Option<String>,
    pub password: Option<String>,
}

/// Basic info for an event.
#[derive(Debug, Clone)]
pub struct Event<'a> {
    pub event_id: Cow<'a, str>,
    pub summary: Option<Cow<'a, str>>,
    pub description: Option<Cow<'a, str>>,
    pub location: Option<Cow<'a, str>>,
}

/// A particular instance of an event, with date/time and attendees.
#[derive(Debug, Clone)]
pub struct EventInstance<'a> {
    pub event_id: Cow<'a, str>,
    pub date: DateTime<FixedOffset>,
    pub attendees: Vec<Attendee>,
}

/// A reminder for a particular [`EventInstance`]
#[derive(Debug, Clone)]
pub struct Reminder {
    pub event_id: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub template: Option<String>,
    pub minutes_before: i64,
    pub room_id: String,
    pub attendees: Vec<Attendee>,
}

/// Allows talking to the database.
#[derive(Debug, Clone)]
pub struct Database {
    db_pool: PostgresPool,
}

impl Database {
    /// Create a new `Database` from a PostgreSQL connection pool.
    pub fn from_pool(db_pool: PostgresPool) -> Database {
        Database { db_pool }
    }

    /// Fetch stored calendar info.
    pub async fn get_calendars(&self) -> Result<Vec<Calendar>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                "SELECT calendar_id, url, user_name, password FROM calendars",
                &[],
            )
            .await?;

        let mut calendars = Vec::with_capacity(rows.len());
        for row in rows {
            let calendar_id: i64 = row.get(0);
            let url: String = row.get(1);
            let user_name: Option<String> = row.get(2);
            let password: Option<String> = row.get(3);

            calendars.push(Calendar {
                calendar_id,
                url,
                user_name,
                password,
            })
        }

        Ok(calendars)
    }

    /// Insert events and the next instances of the event.
    ///
    /// Not all event instances are stored (since they might be infinite),
    /// instead only the instances in the next, say, month are typically stored.
    pub async fn insert_events(
        &self,
        calendar_id: i64,
        events: Vec<Event<'_>>,
        instances: Vec<EventInstance<'_>>,
    ) -> Result<(), Error> {
        let mut db_conn = self.db_pool.get().await?;
        let txn = db_conn.transaction().await?;

        futures::future::try_join_all(events.iter().map(|event| {
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
                    &event.event_id,
                    &event.summary,
                    &event.description,
                    &event.location,
                ],
            )
        }))
        .await?;

        txn.execute(
            "DELETE FROM next_dates WHERE calendar_id = $1",
            &[&calendar_id],
        )
        .await?;

        futures::future::try_join_all(instances.iter().map(|instance| {
            txn.execute_raw(
                r#"
                            INSERT INTO next_dates (calendar_id, event_id, timestamp, attendees)
                            VALUES ($1, $2, $3, $4)
                        "#,
                vec![
                    &calendar_id as &dyn ToSql,
                    &instance.event_id,
                    &instance.date,
                    &instance.attendees,
                ],
            )
        }))
        .await?;

        txn.commit().await?;

        Ok(())
    }

    /// Get the reminders needed to be sent out.
    pub async fn get_next_reminders(&self) -> Result<VecDeque<(DateTime<Utc>, Reminder)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT event_id, summary, description, location, timestamp, room_id, minutes_before, template, attendees
                    FROM reminders
                    INNER JOIN events USING (calendar_id, event_id)
                    INNER JOIN next_dates USING (calendar_id, event_id)
                    ORDER BY timestamp
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
                // XXX: There's technically a race here if we reload the
                // reminders just as we're about to send out a reminder.
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

        Ok(reminders)
    }

    /// Get all events in a calendar
    pub async fn get_events_in_calendar(
        &self,
        calendar_id: i64,
    ) -> Result<Vec<(Event<'static>, EventInstance<'static>)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT DISTINCT ON (event_id) event_id, summary, description, location, timestamp, attendees
                    FROM events
                    INNER JOIN next_dates USING (calendar_id, event_id)
                    WHERE calendar_id = $1
                    ORDER BY event_id, timestamp
                "#,
                &[&calendar_id],
            )
            .await?;

        let mut events = Vec::with_capacity(rows.len());

        for row in rows {
            let event_id: String = row.get(0);
            let summary: Option<String> = row.get(1);
            let description: Option<String> = row.get(2);
            let location: Option<String> = row.get(3);
            let date: DateTime<FixedOffset> = row.get(4);
            let attendees: Vec<Attendee> = row.get(5);

            if date < Utc::now() {
                // ignore events in the past
                continue;
            }

            let event = Event {
                event_id: event_id.clone().into(),
                summary: summary.map(Cow::from),
                description: description.map(Cow::from),
                location: location.map(Cow::from),
            };

            let instance = EventInstance {
                event_id: event_id.into(),
                date,
                attendees,
            };

            events.push((event, instance));
        }

        events.sort_by_key(|(_, i)| i.date);

        Ok(events)
    }

    /// Get the stored mappings from email to matrix ID.
    pub async fn get_user_mappings(&self) -> Result<BTreeMap<String, String>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query("SELECT email, matrix_id FROM email_to_matrix_id", &[])
            .await?;

        let mapping: BTreeMap<String, String> = rows
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();

        Ok(mapping)
    }
}
