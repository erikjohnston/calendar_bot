CREATE TABLE calendars (
    calendar_id BIGSERIAL PRIMARY KEY,
    user_id bigint NOT NULL,
    url text NOT NULL,
    user_name text,
    password text
);


CREATE TABLE events (
    calendar_id bigint NOT NULL,
    event_id text NOT NULL,
    summary text,
    description text,
    location text
);

CREATE UNIQUE INDEX ON events USING btree (calendar_id, event_id);


CREATE TYPE "Attendee" AS (
    email TEXT,
    common_name TEXT
);


CREATE TABLE next_dates (
    calendar_id bigint NOT NULL,
    event_id text NOT NULL,
    "timestamp" timestamp with time zone NOT NULL,
    attendees "Attendee"[] NOT NULL
);

CREATE INDEX ON next_dates USING btree (calendar_id, event_id);


CREATE TABLE reminders (
    reminder_id BIGSERIAL NOT NULL,
    user_id bigint NOT NULL,
    calendar_id bigint NOT NULL,
    event_id text NOT NULL,
    room_id text NOT NULL,
    minutes_before bigint NOT NULL,
    template text
);

CREATE UNIQUE INDEX ON reminders (calendar_id, event_id);

CREATE TABLE users (
    user_id BIGSERIAL NOT NULL,
    matrix_id TEXT NOT NULL
);


CREATE TABLE email_to_matrix_id (
    email TEXT PRIMARY KEY,
    matrix_id TEXT NOT NULL
);
