CREATE TABLE calendars (
    calendar_id BIGSERIAL PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users(user_id),
    name TEXT NOT NULL,
    url text NOT NULL
);

CREATE TABLE calendar_passwords (
    calendar_id BIGINT NOT NULL REFERENCES calendars(calendar_id),
    user_name TEXT NOT NULL,
    password TEXT NOT NULL
);

CREATE UNIQUE INDEX ON calendar_passwords(calendar_id);


CREATE TYPE "Attendee" AS (
    email TEXT,
    common_name TEXT
);


CREATE TABLE events (
    calendar_id bigint NOT NULL REFERENCES calendars(calendar_id),
    event_id text NOT NULL,
    summary text,
    description text,
    location text,
    organizer "Attendee",
    attendees "Attendee"[] NOT NULL
);

CREATE UNIQUE INDEX ON events USING btree (calendar_id, event_id);


CREATE TABLE next_dates (
    calendar_id bigint NOT NULL REFERENCES calendars(calendar_id),
    event_id text NOT NULL,
    "timestamp" timestamp with time zone NOT NULL,
    attendees "Attendee"[] NOT NULL
    FOREIGN KEY (calendar_id, event_id) REFERENCES events (calendar_id, event_id)
);

CREATE INDEX ON next_dates USING btree (calendar_id, event_id);


CREATE TABLE reminders (
    reminder_id BIGSERIAL PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users(user_id),
    calendar_id bigint NOT NULL REFERENCES calendars(calendar_id),
    event_id text NOT NULL,
    room text NOT NULL,
    minutes_before bigint NOT NULL,
    template text,
    attendee_editable boolean NOT NULL,
    FOREIGN KEY (calendar_id, event_id) REFERENCES events (calendar_id, event_id)
);

CREATE INDEX ON reminders(event_id);


CREATE TABLE users (
    user_id BIGSERIAL PRIMARY KEY,
    password_hash TEXT,
    email TEXT NOT NULL
);

CREATE UNIQUE INDEX ON users(email);


CREATE TABLE email_to_matrix_id (
    email TEXT PRIMARY KEY,
    matrix_id TEXT NOT NULL
);

CREATE INDEX ON email_to_matrix_id(matrix_id);

CREATE TABLE access_tokens (
    access_token_id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    token TEXT NOT NULL,
    expiry TIMESTAMP WITH TIME ZONE NOT NULL
);

CREATE UNIQUE INDEX ON access_tokens (token);


CREATE TABLE out_today (
    email TEXT NOT NULL
);

CREATE UNIQUE INDEX ON out_today ( email );


CREATE TABLE sso_sessions (
    crsf_token TEXT NOT NULL,
    nonce TEXT NOT NULL,
    code_verifier TEXT NOT NULL
);

CREATE UNIQUE INDEX ON sso_sessions(crsf_token);


CREATE TABLE oauth2_sessions (
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    crsf_token TEXT NOT NULL,
    code_verifier TEXT NOT NULL,
    path TEXT NOT NULL
);

CREATE UNIQUE INDEX ON oauth2_sessions(crsf_token);


CREATE TABLE oauth2_tokens (
    token_id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    account_id BIGINT NOT NULL REFERENCES oauth2_accounts,
    access_token TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    expiry TIMESTAMP WITH TIME ZONE NOT NULL
);

CREATE INDEX ON oauth2_tokens(user_id);
CREATE INDEX ON oauth2_tokens(expiry);

CREATE TABLE calendar_oauth2 (
    calendar_id BIGINT NOT NULL REFERENCES calendars(calendar_id),
    account_id BIGINT NOT NULL REFERENCES oauth2_accounts
);

CREATE UNIQUE INDEX ON calendar_oauth2(calendar_id);


CREATE TABLE oauth2_accounts (
    account_id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(user_id),
    email TEXT NOT NULL
);

CREATE UNIQUE INDEX ON oauth2_accounts(user_id, email);
