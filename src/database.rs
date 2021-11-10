//! Module for talking to the database

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Deref;

use anyhow::Error;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use postgres_types::{FromSql, ToSql};
use serde::{Deserialize, Serialize};
use tokio_postgres::NoTls;
use tracing::debug;

/// Async database pool for PostgreSQL.
pub type PostgresPool = bb8::Pool<bb8_postgres::PostgresConnectionManager<NoTls>>;

/// An attendee of the meeting.
///
/// Includes people who haven't responded, or are tentative/confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, ToSql, FromSql)]
pub struct Attendee {
    pub email: String,
    pub common_name: Option<String>,
}

/// The URL and credentials of a calendar.
#[derive(Clone, Serialize)]
pub struct Calendar {
    pub user_id: i64,
    pub calendar_id: i64,
    pub name: String,
    pub url: String,
    pub user_name: Option<String>,
    pub password: Option<String>,
}

impl std::fmt::Debug for Calendar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // We format this ourselves as we don't want to leak the password.
        f.debug_struct("Calendar")
            .field("user_id", &self.user_id)
            .field("calendar_id", &self.calendar_id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("user_name", &self.user_name)
            .field("password", &self.password.as_deref().map(|_| "xxxxxxxxx"))
            .finish()
    }
}

/// Basic info for an event.
#[derive(Debug, Clone)]
pub struct Event {
    pub calendar_id: i64,
    pub event_id: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<Attendee>,
    pub attendees: Vec<Attendee>,
}

/// A particular instance of an event, with date/time and attendees.
#[derive(Debug, Clone)]
pub struct EventInstance {
    pub event_id: String,
    pub date: DateTime<FixedOffset>,
    pub attendees: Vec<Attendee>,
}

/// A reminder for a particular [`EventInstance`]
#[derive(Debug, Clone)]
pub struct ReminderInstance {
    pub event_id: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub template: Option<String>,
    pub minutes_before: i64,
    pub room: String,
    pub attendees: Vec<Attendee>,
}

/// A configured reminder
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Reminder {
    pub reminder_id: i64,
    pub calendar_id: i64,
    pub user_id: i64,
    pub event_id: String,
    pub template: Option<String>,
    pub minutes_before: i64,
    pub room: String,
    pub attendee_editable: bool,
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
                "SELECT user_id, calendar_id, name, url, user_name, password FROM calendars",
                &[],
            )
            .await?;

        let mut calendars = Vec::with_capacity(rows.len());
        for row in rows {
            let user_id = row.try_get("user_id")?;
            let calendar_id = row.try_get("calendar_id")?;
            let name = row.try_get("name")?;
            let url = row.try_get("url")?;
            let user_name = row.try_get("user_name")?;
            let password = row.try_get("password")?;

            calendars.push(Calendar {
                user_id,
                calendar_id,
                name,
                url,
                user_name,
                password,
            })
        }

        Ok(calendars)
    }

    /// Get all calendars for a given user.
    pub async fn get_calendars_for_user(&self, user_id: i64) -> Result<Vec<Calendar>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT calendar_id, name, url, user_name, password FROM calendars
                    WHERE user_id = $1
                "#,
                &[&user_id],
            )
            .await?;

        let mut calendars = Vec::with_capacity(rows.len());
        for row in rows {
            let calendar_id = row.try_get("calendar_id")?;
            let name = row.try_get("name")?;
            let url = row.try_get("url")?;
            let user_name = row.try_get("user_name")?;
            let password = row.try_get("password")?;

            calendars.push(Calendar {
                user_id,
                calendar_id,
                name,
                url,
                user_name,
                password,
            })
        }

        Ok(calendars)
    }

    /// Get a calendar by ID.
    pub async fn get_calendar(&self, calendar_id: i64) -> Result<Option<Calendar>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                r#"
                    SELECT user_id, calendar_id, name, url, user_name, password FROM calendars
                    WHERE calendar_id = $1
                "#,
                &[&calendar_id],
            )
            .await?;

        if let Some(row) = row {
            let user_id = row.try_get("user_id")?;
            let calendar_id = row.try_get("calendar_id")?;
            let name = row.try_get("name")?;
            let url = row.try_get("url")?;
            let user_name = row.try_get("user_name")?;
            let password = row.try_get("password")?;

            Ok(Some(Calendar {
                user_id,
                calendar_id,
                name,
                url,
                user_name,
                password,
            }))
        } else {
            Ok(None)
        }
    }

    /// Update a calendar's config.
    pub async fn update_calendar(
        &self,
        calendar_id: i64,
        name: String,
        url: String,
        user_name: Option<String>,
        password: Option<String>,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                    UPDATE calendars
                    SET name = $2, url = $3, user_name = $4, password = $5
                    WHERE calendar_id = $1
                "#,
                &[&calendar_id, &name, &url, &user_name, &password],
            )
            .await?;

        Ok(())
    }

    /// Delete a calendar.
    pub async fn delete_calendar(&self, calendar_id: i64) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                    DELETE FROM calendars
                    WHERE calendar_id = $1
                "#,
                &[&calendar_id],
            )
            .await?;

        Ok(())
    }

    /// Add a new calendar.
    pub async fn add_calendar(
        &self,
        user_id: i64,
        name: String,
        url: String,
        user_name: Option<String>,
        password: Option<String>,
    ) -> Result<i64, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_one(
                r#"
                    INSERT INTO calendars (user_id, name, url, user_name, password)
                    VALUES ($1, $2, $3, $4, $5)
                    RETURNING calendar_id
                "#,
                &[&user_id, &name, &url, &user_name, &password],
            )
            .await?;

        Ok(row.try_get(0)?)
    }

    /// Insert events and the next instances of the event.
    ///
    /// Not all event instances are stored (since they might be infinite),
    /// instead only the instances in the next, say, month are typically stored.
    pub async fn insert_events(
        &self,
        calendar_id: i64,
        events: Vec<Event>,
        instances: Vec<EventInstance>,
    ) -> Result<(), Error> {
        let mut db_conn = self.db_pool.get().await?;
        let txn = db_conn.transaction().await?;

        futures::future::try_join_all(events.iter().map(|event| {
            txn.execute_raw(
                r#"
                    INSERT INTO events (calendar_id, event_id, summary, description, location, organizer, attendees)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                    ON CONFLICT (calendar_id, event_id)
                    DO UPDATE SET
                        summary = EXCLUDED.summary,
                        description = EXCLUDED.description,
                        location = EXCLUDED.location,
                        attendees = EXCLUDED.attendees
                "#,
                vec![
                    &calendar_id as &dyn ToSql,
                    &event.event_id,
                    &event.summary,
                    &event.description,
                    &event.location,
                    &event.organizer,
                    &event.attendees,
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

    /// Persist a new reminder.
    pub async fn add_reminder(&self, reminder: Reminder) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                    INSERT INTO reminders (
                        user_id, calendar_id, event_id, room,
                        minutes_before, template, attendee_editable
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
                &[
                    &reminder.user_id,
                    &reminder.calendar_id,
                    &reminder.event_id,
                    &reminder.room,
                    &reminder.minutes_before,
                    &reminder.template,
                    &reminder.attendee_editable,
                ],
            )
            .await?;

        Ok(())
    }

    /// Update an existing reminder.
    pub async fn update_reminder(
        &self,
        calendar_id: i64,
        reminder_id: i64,
        room: &'_ str,
        minutes_before: i64,
        template: Option<&'_ str>,
        attendee_editable: bool,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                    UPDATE reminders
                    SET room = $1, minutes_before = $2, template = $3,
                    attendee_editable = $4
                    WHERE calendar_id = $5 AND reminder_id = $6
            "#,
                &[
                    &room,
                    &minutes_before,
                    &template,
                    &attendee_editable,
                    &calendar_id,
                    &reminder_id,
                ],
            )
            .await?;

        Ok(())
    }

    /// Delete a specific reminder.
    pub async fn delete_reminder_in_calendar(
        &self,
        calendar_id: i64,
        reminder_id: i64,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                    DELETE FROM reminders
                    WHERE calendar_id = $1 AND reminder_id = $2
            "#,
                &[&calendar_id, &reminder_id],
            )
            .await?;

        Ok(())
    }

    /// Get the reminders needed to be sent out.
    pub async fn get_next_reminders(
        &self,
    ) -> Result<VecDeque<(DateTime<Utc>, ReminderInstance)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT event_id, summary, description, location, timestamp, room, minutes_before, template, i.attendees
                    FROM reminders
                    INNER JOIN events USING (calendar_id, event_id)
                    INNER JOIN next_dates AS i USING (calendar_id, event_id)
                    ORDER BY timestamp
                "#,
                &[],
            )
            .await?;

        let mut reminders = VecDeque::with_capacity(rows.len());
        let now = Utc::now();

        for row in rows {
            let event_id: String = row.get(0);
            let summary: Option<String> = row.get(1);
            let description: Option<String> = row.get(2);
            let location: Option<String> = row.get(3);
            let timestamp: DateTime<Utc> = row.get(4);
            let room: String = row.get(5);
            let minutes_before: i64 = row.get(6);
            let template: Option<String> = row.get(7);
            let attendees: Vec<Attendee> = row.get(8);

            let reminder_time = timestamp - Duration::minutes(minutes_before);
            if reminder_time < now {
                // XXX: There's technically a race here if we reload the
                // reminders just as we're about to send out a reminder.
                debug!(now = ?now, reminder_time =?reminder_time, event_id = event_id.deref(), "Ignoring reminder");
                continue;
            }

            let reminder = ReminderInstance {
                event_id,
                summary,
                description,
                location,
                template,
                minutes_before,
                room,
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
    ) -> Result<Vec<(Event, Vec<EventInstance>)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT DISTINCT ON (event_id) event_id, summary, description, location, timestamp,
                        organizer, e.attendees AS event_attendees, i.attendees AS instance_attendees
                    FROM events AS e
                    INNER JOIN next_dates AS i USING (calendar_id, event_id)
                    WHERE calendar_id = $1
                    ORDER BY event_id, timestamp
                "#,
                &[&calendar_id],
            )
            .await?;

        let mut events: Vec<(Event, Vec<EventInstance>)> = Vec::with_capacity(rows.len());

        for row in rows {
            let event_id: String = row.try_get("event_id")?;
            let summary = row.try_get("summary")?;
            let description = row.try_get("description")?;
            let location = row.try_get("location")?;
            let date = row.try_get("timestamp")?;
            let organizer = row.try_get("organizer")?;
            let instance_attendees = row.try_get("instance_attendees")?;
            let event_attendees = row.try_get("event_attendees")?;

            if date < Utc::now() {
                // ignore events in the past
                continue;
            }

            let instance = EventInstance {
                event_id: event_id.clone(),
                date,
                attendees: instance_attendees,
            };

            if let Some((event, instances)) = events.last_mut() {
                if event.event_id == event_id {
                    instances.push(instance);
                    continue;
                }
            }

            let event = Event {
                calendar_id,
                event_id,
                summary,
                description,
                location,
                organizer,
                attendees: event_attendees,
            };
            events.push((event, vec![instance]));
        }

        events.sort_by_key(|(_, i)| i[0].date);

        Ok(events)
    }

    /// Get all events from all a given user's calendars.
    pub async fn get_events_for_user(
        &self,
        user_id: i64,
    ) -> Result<Vec<(Event, Vec<EventInstance>)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT DISTINCT ON (calendar_id, event_id) calendar_id, event_id, summary, description, location, timestamp,
                        organizer, e.attendees AS event_attendees, i.attendees AS instance_attendees
                    FROM calendars
                    INNER JOIN events AS e USING (calendar_id)
                    INNER JOIN next_dates AS i USING (calendar_id, event_id)
                    WHERE user_id = $1
                    ORDER BY calendar_id, event_id, timestamp
                "#,
                &[&user_id],
            )
            .await?;

        let mut events: Vec<(Event, Vec<EventInstance>)> = Vec::with_capacity(rows.len());

        for row in rows {
            let calendar_id = row.try_get("calendar_id")?;
            let event_id: String = row.try_get("event_id")?;
            let summary = row.try_get("summary")?;
            let description = row.try_get("description")?;
            let location = row.try_get("location")?;
            let date = row.try_get("timestamp")?;
            let organizer = row.try_get("organizer")?;
            let instance_attendees = row.try_get("instance_attendees")?;
            let event_attendees = row.try_get("event_attendees")?;

            if date < Utc::now() {
                // ignore events in the past
                continue;
            }

            let instance = EventInstance {
                event_id: event_id.clone(),
                date,
                attendees: instance_attendees,
            };

            if let Some((event, instances)) = events.last_mut() {
                if event.event_id == event_id {
                    instances.push(instance);
                    continue;
                }
            }

            let event = Event {
                calendar_id,
                event_id,
                summary,
                description,
                location,
                organizer,
                attendees: event_attendees,
            };
            events.push((event, vec![instance]));
        }

        events.sort_by_key(|(_, i)| i[0].date);

        Ok(events)
    }

    /// Get all events for user that have reminders
    pub async fn get_events_with_reminders(
        &self,
        user_id: i64,
    ) -> Result<Vec<(Event, Vec<EventInstance>)>, Error> {
        let events = self.get_events_for_user(user_id).await?;

        let mut filtered_events = Vec::with_capacity(events.len());
        for (event, instance) in events {
            let reminders = self
                .get_reminders_for_event(event.calendar_id, &event.event_id)
                .await?;

            if !reminders.is_empty() {
                filtered_events.push((event, instance))
            }
        }

        Ok(filtered_events)
    }

    /// Get the specified event
    pub async fn get_event_in_calendar(
        &self,
        calendar_id: i64,
        event_id: &str,
    ) -> Result<Option<(Event, Vec<EventInstance>)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                r#"
                    SELECT DISTINCT ON (event_id) event_id, summary, description, location,
                        organizer, attendees
                    FROM events
                    WHERE calendar_id = $1 AND event_id = $2
                "#,
                &[&calendar_id, &event_id],
            )
            .await?;

        let row = if let Some(row) = row {
            row
        } else {
            return Ok(None);
        };

        let event_id: String = row.try_get("event_id")?;
        let summary = row.try_get("summary")?;
        let description = row.try_get("description")?;
        let location = row.try_get("location")?;
        let attendees = row.try_get("attendees")?;
        let organizer = row.try_get("organizer")?;

        let event = Event {
            calendar_id,
            event_id: event_id.clone(),
            summary,
            description,
            location,
            attendees,
            organizer,
        };

        let mut instances = Vec::new();

        let rows = db_conn
            .query(
                r#"
                    SELECT timestamp, attendees
                    FROM next_dates
                    WHERE calendar_id = $1 AND event_id = $2
                    ORDER BY timestamp
                "#,
                &[&calendar_id, &event_id],
            )
            .await?;

        for row in rows {
            let date: DateTime<FixedOffset> = row.get("timestamp");
            let attendees: Vec<Attendee> = row.get("attendees");

            if date < Utc::now() {
                // ignore events in the past
                continue;
            }

            let instance = EventInstance {
                event_id: event_id.clone(),
                date,
                attendees,
            };

            instances.push(instance);
        }

        instances.sort_by_key(|i| i.date);

        Ok(Some((event, instances)))
    }

    /// Get reminders for the event, including reminders in other people's
    /// calendars that are shared.
    pub async fn get_reminders_for_event(
        &self,
        calendar_id: i64,
        event_id: &str,
    ) -> Result<Vec<Reminder>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT DISTINCT ON (reminder_id) reminders.calendar_id, reminders.user_id, reminder_id, room,
                        minutes_before, attendee_editable, template
                    FROM (
                        SELECT user_id, calendar_id, event_id, attendees
                        FROM events
                        INNER JOIN calendars USING (calendar_id)
                        WHERE event_id = $1
                    ) AS c
                    INNER JOIN users USING (user_id)
                    INNER JOIN reminders USING (event_id)
                    WHERE
                        c.calendar_id = $2
                        AND (
                            -- Either the event is in their own calendar...
                            reminders.calendar_id = $2
                            --- Or the reminder is attendee editable and they are an attendee
                            OR (attendee_editable AND users.matrix_id IN (
                                SELECT e.matrix_id FROM UNNEST(attendees) AS a
                                INNER JOIN email_to_matrix_id AS e USING (email)
                            ))
                        )
                    "#,
                &[&event_id, &calendar_id],
            )
            .await?;

        let mut reminders = Vec::with_capacity(rows.len());
        for row in rows {
            let reminder_calendar_id = row.try_get("calendar_id")?;
            let user_id = row.try_get("user_id")?;
            let reminder_id = row.try_get("reminder_id")?;
            let room = row.try_get("room")?;
            let minutes_before = row.try_get("minutes_before")?;
            let template = row.try_get("template")?;
            let attendee_editable = row.try_get("attendee_editable")?;

            let reminder = Reminder {
                reminder_id,
                user_id,
                calendar_id: reminder_calendar_id,
                event_id: event_id.to_string(),
                room,
                minutes_before,
                template,
                attendee_editable,
            };
            reminders.push(reminder)
        }

        Ok(reminders)
    }

    /// Get the list of users that can edit an event.
    ///
    /// This is the owner of the reminder, and if the `attendee_editable` flag
    /// is set, all attendees.
    pub async fn get_users_who_can_edit_reminder(
        &self,
        reminder_id: i64,
    ) -> Result<Vec<i64>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                    SELECT c.user_id FROM (
                        SELECT user_id, calendar_id, event_id, attendees
                        FROM events
                        INNER JOIN calendars USING (calendar_id)
                    ) AS c
                    INNER JOIN users USING (user_id)
                    INNER JOIN reminders USING (event_id)
                    WHERE reminder_id = $1
                        AND (
                            -- Either the event is in their own calendar...
                            reminders.calendar_id = c.calendar_id
                            --- Or the reminder is attendee editable and they are an attendee
                            OR (attendee_editable AND users.matrix_id IN (
                                SELECT e.matrix_id FROM UNNEST(attendees) AS a
                                INNER JOIN email_to_matrix_id AS e USING (email)
                            ))
                        )
                    "#,
                &[&reminder_id],
            )
            .await?;

        let mut users = Vec::with_capacity(rows.len());

        for row in rows {
            let user_id = row.try_get("user_id")?;
            users.push(user_id);
        }

        Ok(users)
    }

    /// Get a reminder for event
    pub async fn get_reminder_in_calendar(
        &self,
        calendar_id: i64,
        reminder_id: i64,
    ) -> Result<Option<Reminder>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                r#"
                    SELECT calendar_id, event_id, user_id, reminder_id, room, minutes_before,
                        template, attendee_editable
                    FROM reminders
                    WHERE calendar_id = $1 AND reminder_id = $2
                "#,
                &[&calendar_id, &reminder_id],
            )
            .await?;

        let row = if let Some(row) = row {
            row
        } else {
            return Ok(None);
        };

        let calendar_id = row.try_get("calendar_id")?;
        let reminder_id = row.try_get("reminder_id")?;
        let user_id = row.try_get("user_id")?;
        let event_id = row.try_get("event_id")?;
        let room = row.try_get("room")?;
        let minutes_before = row.try_get("minutes_before")?;
        let template = row.try_get("template")?;
        let attendee_editable = row.try_get("attendee_editable")?;

        let reminder = Reminder {
            reminder_id,
            calendar_id,
            user_id,
            event_id,
            template,
            minutes_before,
            room,
            attendee_editable,
        };

        Ok(Some(reminder))
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

    /// Check the password matches the hash in the DB for the user with given
    /// Matrix ID.
    pub async fn check_password(
        &self,
        matrix_id: &str,
        password: &str,
    ) -> Result<Option<i64>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                "SELECT user_id, password_hash FROM users WHERE matrix_id = $1",
                &[&matrix_id],
            )
            .await?;

        let (user_id, hash) = if let Some(row) = row {
            let user_id: i64 = row.try_get(0)?;
            let hash: String = row.try_get(1)?;
            (user_id, hash)
        } else {
            return Ok(None);
        };

        if bcrypt::verify(password, &hash)? {
            Ok(Some(user_id))
        } else {
            Ok(None)
        }
    }

    /// Check password matches the hash in the DB of the given user.
    pub async fn check_password_user_id(
        &self,
        user_id: i64,
        password: &str,
    ) -> Result<Option<()>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                "SELECT password_hash FROM users WHERE user_id = $1",
                &[&user_id],
            )
            .await?;

        let hash = if let Some(row) = row {
            let hash: String = row.try_get("password_hash")?;
            hash
        } else {
            return Ok(None);
        };

        if bcrypt::verify(password, &hash)? {
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    /// Update the password for the users.
    pub async fn change_password(&self, user_id: i64, password_hash: &str) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                "UPDATE users SET password_hash = $1 WHERE user_id = $2",
                &[&password_hash, &user_id],
            )
            .await?;

        Ok(())
    }

    /// Add an access token for the user.
    pub async fn add_access_token(
        &self,
        user_id: i64,
        token: &str,
        expiry: DateTime<Utc>,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                "INSERT INTO access_tokens (user_id, token, expiry) VALUES ($1, $2, $3)",
                &[&user_id, &token, &expiry],
            )
            .await?;

        Ok(())
    }

    /// Get the user associated with the access token.
    pub async fn get_user_from_token(&self, token: &str) -> Result<Option<i64>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                "SELECT user_id FROM access_tokens WHERE token = $1 AND expiry > NOW()",
                &[&token],
            )
            .await?;

        if let Some(row) = row {
            Ok(Some(row.try_get(0)?))
        } else {
            Ok(None)
        }
    }

    /// Persist all emails that are on holiday today.
    pub async fn set_out_today(&self, emails: &[String]) -> Result<(), Error> {
        let mut db_conn = self.db_pool.get().await?;

        let txn = db_conn.transaction().await?;

        txn.execute("TRUNCATE out_today", &[]).await?;

        futures::future::try_join_all(
            emails
                .iter()
                .map(|email| txn.execute_raw("INSERT INTO out_today VALUES ($1)", vec![email])),
        )
        .await?;

        txn.commit().await?;

        Ok(())
    }

    /// Get all emails that are on holiday today.
    pub async fn get_out_today_emails(&self) -> Result<BTreeSet<String>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn.query("SELECT email FROM out_today", &[]).await?;

        Ok(rows.into_iter().map(|row| row.get(0)).collect())
    }

    /// Persist a email to matrix ID mapping.
    ///
    /// This does *not* overwrite existing mappings. Returns true if the new
    /// mapping was added.
    pub async fn add_matrix_id(&self, email: &str, matrix_id: &str) -> Result<bool, Error> {
        let db_conn = self.db_pool.get().await?;

        let ret = db_conn
            .query_opt(
                r#"
                INSERT INTO email_to_matrix_id (email, matrix_id) VALUES ($1, $2)
                ON CONFLICT DO NOTHING
                RETURNING 1
                "#,
                &[&email, &matrix_id],
            )
            .await?;

        Ok(ret.is_some())
    }
}
