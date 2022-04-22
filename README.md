# Calendar Reminder Matrix Bot

A Matrix bot that notifies channels based on users' calendars.

## Prerequisites

* Install [Rust](https://www.rust-lang.org/tools/install).
* Install [PostgreSQL](https://www.postgresql.org/).
* Create a user and database in PostgreSQL:

```bash
sudo -u postgres createuser --pwprompt --createdb calbot
createdb --host=localhost --username=calbot calbotdb
psql --host=localhost --username=calbot calbotdb < database.sql
```

* Create a Matrix user and get the user's access token.

## Building

```bash
cargo test
```

## Running

### First time

Create a config file with:

```bash
cp config.sample.toml config.toml
```

and edit `config.toml` to set up your Postgres database and Matrix credentials.

Now create a user:

```bash
cargo run -- create-user myname mypassword
```

### Every time

```bash
cargo run
```

Now you can access the web UI on http://127.0.0.1:8080 or a different address
if you provided a `bind_addr` in the `app` section of your config. You can log
in using the credentials you provided to `create-user` above ("myname" and
"mypassword").
