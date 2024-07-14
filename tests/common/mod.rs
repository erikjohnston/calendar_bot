use actix_http::Request;
use actix_web::{
    body::MessageBody,
    cookie::Cookie,
    dev::{Service, ServiceResponse},
    middleware::Logger,
};
use anyhow::Error;
use calendar_bot::config::Config;
use pgtemp::PgTempDB;
use tracing_actix_web::TracingLogger;

pub async fn create_actix_app() -> Result<
    (
        calendar_bot::app::App,
        PgTempDB,
        impl Service<Request, Response = ServiceResponse<impl MessageBody>, Error = actix_web::Error>,
    ),
    Error,
> {
    let db = PgTempDB::async_new().await;
    db.load_database("database.sql");
    let db_conn_str = db.connection_string();

    let config: Config = toml::from_str(&format!(
        r#"
        [database]
        connection_string = "{db_conn_str}"

        [matrix]
        homeserver_url = ""
        access_token = ""
    "#
    ))?;

    let app = calendar_bot::create_app(config).await?;

    let actix_app = actix_web::test::init_service(
        actix_web::App::new()
            .wrap(TracingLogger::default())
            .wrap(Logger::default())
            .app_data(actix_web::web::Data::new(app.clone()))
            .configure(calendar_bot::site::add_services),
    )
    .await;

    Ok((app, db, actix_app))
}

pub async fn create_user_and_login(
    app: &calendar_bot::app::App,
    username: &str,
) -> Result<Cookie<'static>, Error> {
    let user_id: i64 = app.database.upsert_account(username).await?;
    let token = app.add_access_token(user_id).await?;

    let cookie = Cookie::build("token", token).finish();

    Ok(cookie)
}
