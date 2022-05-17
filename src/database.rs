//! Module for talking to the database

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Deref;

use anyhow::{Context, Error};
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

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum CalendarAuthentication {
    None,
    Basic { user_name: String, password: String },
    Bearer { access_token: String },
}

impl std::fmt::Debug for CalendarAuthentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalendarAuthentication::None => write!(f, "None"),
            CalendarAuthentication::Basic {
                user_name,
                password: _,
            } => f
                .debug_struct("Basic")
                .field("user_name", user_name)
                .field("password", &"<password>")
                .finish(),
            CalendarAuthentication::Bearer { .. } => f.debug_struct("Bearer").finish(),
        }
    }
}

/// The URL and credentials of a calendar.
#[derive(Debug, Clone, Serialize)]
pub struct Calendar {
    pub user_id: i64,
    pub calendar_id: i64,
    pub name: String,
    pub url: String,

    #[serde(skip)]
    pub authentication: CalendarAuthentication,
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

/// Result of requesting an OAuth2 access token from the DB.
pub enum OAuth2Result {
    /// User hasn't authenticated yet
    None,

    /// User has authenticated, but we need to refresh the token
    RefreshToken {
        refresh_token: String,
        token_id: i64,
    },

    /// User has a (probably) valid access token
    AccessToken(String),
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
                r#"
                SELECT
                    c.user_id, c.calendar_id, c.name, c.url,
                    cp.user_name, cp.password,
                    at.access_token
                FROM calendars AS c
                LEFT JOIN calendar_passwords AS cp USING (calendar_id)
                LEFT JOIN calendar_oauth2 AS co USING (calendar_id)
                LEFT JOIN oauth2_tokens AS at USING (token_id)
                "#,
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

            let access_token = row.try_get("access_token")?;

            let authentication = if let (Some(user_name), Some(password)) = (user_name, password) {
                CalendarAuthentication::Basic {
                    user_name,
                    password,
                }
            } else if let Some(access_token) = access_token {
                CalendarAuthentication::Bearer { access_token }
            } else {
                CalendarAuthentication::None
            };

            calendars.push(Calendar {
                user_id,
                calendar_id,
                name,
                url,
                authentication,
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
                SELECT calendar_id, name, url, cp.user_name, cp.password
                FROM calendars
                LEFT JOIN calendar_passwords AS cp USING (calendar_id)
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

            let authentication = if let (Some(user_name), Some(password)) = (user_name, password) {
                CalendarAuthentication::Basic {
                    user_name,
                    password,
                }
            } else {
                CalendarAuthentication::None
            };

            calendars.push(Calendar {
                user_id,
                calendar_id,
                name,
                url,
                authentication,
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
                    SELECT user_id, calendar_id, name, url, cp.user_name, cp.password
                    FROM calendars
                    LEFT JOIN calendar_passwords AS cp USING (calendar_id)
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

            let authentication = if let (Some(user_name), Some(password)) = (user_name, password) {
                CalendarAuthentication::Basic {
                    user_name,
                    password,
                }
            } else {
                CalendarAuthentication::None
            };

            Ok(Some(Calendar {
                user_id,
                calendar_id,
                name,
                url,
                authentication,
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
        let mut db_conn = self.db_pool.get().await?;

        let txn = db_conn.transaction().await?;

        txn.execute(
            r#"
                    UPDATE calendars
                    SET name = $2, url = $3
                    WHERE calendar_id = $1
                "#,
            &[&calendar_id, &name, &url],
        )
        .await?;

        if let (Some(user_name), Some(password)) = (user_name, password) {
            txn.execute(
                r#"
                        UPDATE calendar_passwords
                        SET user_name = $2, password = $3
                        WHERE calendar_id = $1
                    "#,
                &[&calendar_id, &user_name, &password],
            )
            .await?;
        } else {
            txn.execute(
                r#"
                        DELETE FROM calendar_passwords
                        WHERE calendar_id = $1
                    "#,
                &[&calendar_id],
            )
            .await?;
        }

        txn.commit().await?;

        Ok(())
    }

    /// Delete a calendar.
    pub async fn delete_calendar(&self, calendar_id: i64) -> Result<(), Error> {
        let mut db_conn = self.db_pool.get().await?;

        let txn = db_conn.transaction().await?;

        txn.execute(
            r#"
                    DELETE FROM calendar_passwords
                    WHERE calendar_id = $1
                "#,
            &[&calendar_id],
        )
        .await?;

        txn.execute(
            r#"
                    DELETE FROM calendars
                    WHERE calendar_id = $1
                "#,
            &[&calendar_id],
        )
        .await?;

        txn.commit().await?;

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
        let mut db_conn = self.db_pool.get().await?;

        let txn = db_conn.transaction().await?;

        let row = txn
            .query_one(
                r#"
                    INSERT INTO calendars (user_id, name, url)
                    VALUES ($1, $2, $3)
                    RETURNING calendar_id
                "#,
                &[&user_id, &name, &url],
            )
            .await?;

        let calendar_id = row.try_get(0)?;

        if let (Some(user_name), Some(password)) = (user_name, password) {
            txn.execute(
                r#"
                    INSERT INTO calendars (calendar_id, user_name, password)
                    VALUES ($1, $2, $3)
                "#,
                &[&calendar_id, &user_name, &password],
            )
            .await?;
        }

        txn.commit().await?;

        Ok(calendar_id)
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
                            OR (attendee_editable AND users.email IN (
                                SELECT email FROM UNNEST(attendees)
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
                            OR (attendee_editable AND users.email IN (
                                SELECT email FROM UNNEST(attendees)
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

    /// Return the email of this user, or an error if the user does
    /// not exist.
    pub async fn get_email(&self, user_id: i64) -> Result<String, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt("SELECT email FROM users WHERE user_id = $1", &[&user_id])
            .await?;

        if let Some(row) = row {
            let email: String = row.try_get(0)?;
            Ok(email)
        } else {
            Err(anyhow::anyhow!("No user with that user_id"))
        }
    }

    /// Return the Matrix ID of this user, or None if no Matrix ID is mapped
    /// for this user, or an error if the user does not exist.
    pub async fn get_matrix_id(&self, user_id: i64) -> Result<Option<String>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                r#"
                    SELECT email_to_matrix_id.matrix_id
                    FROM email_to_matrix_id
                    INNER JOIN users USING (email)
                    WHERE users.user_id = $1
                "#,
                &[&user_id],
            )
            .await?;

        if let Some(row) = row {
            let matrix_id: String = row.try_get(0)?;
            Ok(Some(matrix_id))
        } else {
            Ok(None)
        }
    }

    /// Check the password matches the hash in the DB for the user with given
    /// Matrix ID.
    pub async fn check_password(&self, email: &str, password: &str) -> Result<Option<i64>, Error> {
        let db_conn = self.db_pool.get().await?;

        let row = db_conn
            .query_opt(
                "SELECT user_id, password_hash FROM users WHERE email = $1",
                &[&email],
            )
            .await?;

        let (user_id, hash) = if let Some(row) = row {
            let user_id: i64 = row.try_get(0)?;
            let hash: Option<String> = row.try_get(1)?;
            (user_id, hash.context("No password found")?)
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
            let hash: Option<String> = row.try_get("password_hash")?;

            hash.context("No password found")?
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
    pub async fn change_password(&self, user_id: i64, password: &str) -> Result<(), Error> {
        let password = password.to_string();
        let password_hash = tokio::task::spawn_blocking(|| bcrypt::hash(password, 12)).await??;

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

    /// Get all matrix IDs that are on holiday today.
    pub async fn get_out_today_matrix_ids(&self) -> Result<BTreeSet<String>, Error> {
        let db_conn = self.db_pool.get().await?;

        let rows = db_conn
            .query(
                r#"
                SELECT matrix_id FROM out_today
                INNER JOIN email_to_matrix_id USING (email)
                "#,
                &[],
            )
            .await?;

        Ok(rows.into_iter().map(|row| row.get(0)).collect())
    }

    /// Persist an email to matrix ID mapping.
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

    /// Persist an email to matrix ID mapping.
    ///
    /// This *does* overwrite existing mappings.
    pub async fn replace_matrix_id(&self, email: &str, matrix_id: &str) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .query_opt(
                r#"
                INSERT INTO email_to_matrix_id (email, matrix_id) VALUES ($1, $2)
                ON CONFLICT (email)
                DO UPDATE SET
                    matrix_id = EXCLUDED.matrix_id
                "#,
                &[&email, &matrix_id],
            )
            .await?;

        Ok(())
    }

    /// Record a new in flight SSO session.
    pub async fn add_sso_session(
        &self,
        crsf_token: &str,
        nonce: &str,
        code_verifier: &str,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                INSERT INTO sso_sessions (crsf_token, nonce, code_verifier) VALUES ($1, $2, $3)
                "#,
                &[&crsf_token, &nonce, &code_verifier],
            )
            .await?;

        Ok(())
    }

    /// Fetch (and delete) an in flight SSO session based on the given token,
    /// returning the stored nonce and code_verifier.
    pub async fn claim_sso_session(
        &self,
        crsf_token: &str,
    ) -> Result<Option<(String, String)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let ret = db_conn
            .query_opt(
                r#"
                DELETE FROM sso_sessions
                WHERE crsf_token = $1
                RETURNING nonce, code_verifier
                "#,
                &[&crsf_token],
            )
            .await?;

        if let Some(row) = ret {
            let nonce: String = row.get(0);
            let code_verifier: String = row.get(1);

            return Ok(Some((nonce, code_verifier)));
        }

        Ok(None)
    }

    /// Creates a new account if one doesn't exist for the email. Returns the
    /// user ID.
    pub async fn upsert_account(&self, email: &str) -> Result<i64, Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                INSERT INTO users (email) VALUES ($1)
                ON CONFLICT DO NOTHING
                "#,
                &[&email],
            )
            .await?;

        let ret = db_conn
            .query_one(
                r#"
                SELECT user_id FROM users
                WHERE email = $1
                "#,
                &[&email],
            )
            .await?;

        let user_id = ret.get("user_id");

        Ok(user_id)
    }

    pub async fn add_google_oauth_token(
        &self,
        user_id: i64,
        access_token: &str,
        refresh_token: &str,
        expiry: DateTime<Utc>,
    ) -> Result<(), Error> {
        let mut db_conn = self.db_pool.get().await?;

        let txn = db_conn.transaction().await?;

        // We only want one oauth2 token per user provisioned at a time, so we
        // delete any existing ones.
        txn.execute(
            r#"DELETE FROM oauth2_tokens WHERE user_id = $1"#,
            &[&user_id],
        )
        .await?;

        txn.execute(
            r#"
            INSERT INTO oauth2_tokens (user_id, access_token, refresh_token, expiry)
            VALUES ($1, $2, $3, $4)
            "#,
            &[&user_id, &access_token, &refresh_token, &expiry],
        )
        .await?;

        txn.commit().await?;

        Ok(())
    }

    pub async fn update_google_oauth_token(
        &self,
        user_id: i64,
        token_id: i64,
        access_token: &str,
        expiry: DateTime<Utc>,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
            UPDATE oauth2_tokens
            SET access_token = $3, expiry = $4
            WHERE token_id = $1 AND user_id = $2
            "#,
                &[&token_id, &user_id, &access_token, &expiry],
            )
            .await?;

        Ok(())
    }

    /// Record a new in flight OAuth2 session.
    pub async fn add_oauth2_session(
        &self,
        user_id: i64,
        crsf_token: &str,
        code_verifier: &str,
        path: &str,
    ) -> Result<(), Error> {
        let db_conn = self.db_pool.get().await?;

        db_conn
            .execute(
                r#"
                INSERT INTO oauth2_sessions (user_id, crsf_token, code_verifier, path) VALUES ($1, $2, $3, $4)
                "#,
                &[&user_id, &crsf_token, &code_verifier, &path],
            )
            .await?;

        Ok(())
    }

    /// Fetch (and delete) an in flight OAuth2 session based on the given token,
    /// returning the associated user ID and code_verifier.
    pub async fn claim_oauth2_session(
        &self,
        crsf_token: &str,
    ) -> Result<Option<(i64, String, String)>, Error> {
        let db_conn = self.db_pool.get().await?;

        let ret = db_conn
            .query_opt(
                r#"
                DELETE FROM oauth2_sessions
                WHERE crsf_token = $1
                RETURNING user_id, code_verifier, path
                "#,
                &[&crsf_token],
            )
            .await?;

        if let Some(row) = ret {
            let user_id: i64 = row.get(0);
            let code_verifier: String = row.get(1);
            let path: String = row.get(2);

            return Ok(Some((user_id, code_verifier, path)));
        }

        Ok(None)
    }

    pub async fn get_oauth2_access_token(&self, user_id: i64) -> Result<OAuth2Result, Error> {
        let db_conn = self.db_pool.get().await?;

        let ret = db_conn
            .query_opt(
                r#"
                SELECT token_id, access_token, refresh_token, expiry
                FROM oauth2_tokens
                WHERE user_id = $1
            "#,
                &[&user_id],
            )
            .await?;

        if let Some(row) = ret {
            let token_id: i64 = row.try_get("token_id")?;
            let access_token: String = row.try_get("access_token")?;
            let refresh_token: String = row.try_get("refresh_token")?;
            let expiry: DateTime<Utc> = row.try_get("expiry")?;

            if expiry < Utc::now() {
                Ok(OAuth2Result::AccessToken(access_token))
            } else {
                Ok(OAuth2Result::RefreshToken {
                    refresh_token,
                    token_id,
                })
            }
        } else {
            Ok(OAuth2Result::None)
        }
    }
}
