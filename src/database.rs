use std::collections::{BTreeMap, VecDeque};

use anyhow::Error;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use postgres_types::{FromSql, ToSql};
use tokio_postgres::NoTls;

pub type PostgresPool = bb8::Pool<bb8_postgres::PostgresConnectionManager<NoTls>>;

#[derive(Debug, Clone, ToSql, FromSql)]
pub struct Attendee {
    pub email: String,
    pub common_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Calendar {
    pub calendar_id: i64,
    pub url: String,
    pub user_name: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Event<'a> {
    pub event_id: &'a str,
    pub summary: Option<&'a str>,
    pub description: Option<&'a str>,
    pub location: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct EventInstance<'a> {
    pub event_id: &'a str,
    pub date: DateTime<FixedOffset>,
    pub attendees: Vec<Attendee>,
}

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

#[derive(Debug, Clone)]
pub struct Database {
    db_pool: PostgresPool,
}

impl Database {
    pub fn from_pool(db_pool: PostgresPool) -> Database {
        Database { db_pool }
    }

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
