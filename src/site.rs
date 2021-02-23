//! The web site for the app.

use actix_web::{
    error::ErrorInternalServerError,
    get,
    middleware::Logger,
    web::{Data, Path},
    HttpServer, Responder,
};
use anyhow::Error;
use itertools::Itertools;
use tracing::info;
use tracing_actix_web::TracingLogger;

use crate::app::App;

/// The index page
#[get("/")]
async fn index() -> impl Responder {
    info!("HELLO");
    "Hello!"
}

/// List *all* configured reminders
#[get("/admin/all_reminders")]
async fn next_reminders(app: Data<App>) -> Result<impl Responder, actix_web::Error> {
    let reminders = app
        .database
        .get_next_reminders()
        .await
        .map_err(ErrorInternalServerError)?;

    let result = reminders
        .into_iter()
        .map(|(date, reminder)| {
            format!(
                "Summary: {}, Date: {}",
                reminder.summary.as_deref().unwrap_or("<null>"),
                date
            )
        })
        .join("\n\n");

    Ok(result)
}

/// List all events in a calendar
#[get("/admin/all_events/{calendar_id}")]
async fn events(app: Data<App>, path: Path<(i64,)>) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();

    let events = app
        .database
        .get_events_in_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let result = events
        .into_iter()
        .map(|(event, instance)| {
            format!(
                "Summary: {}, Date: {}",
                event.summary.as_deref().unwrap_or("<null>"),
                instance.date
            )
        })
        .join("\n\n");

    Ok(result)
}

/// Run the HTTP server.
pub async fn run_server(app: App) -> Result<(), Error> {
    let bind_addr = app
        .config
        .web
        .bind_addr
        .as_deref()
        .unwrap_or("127.0.0.1:8080")
        .to_string();

    HttpServer::new(move || {
        actix_web::App::new()
            .data(app.clone())
            .wrap(TracingLogger)
            .wrap(Logger::default())
            .service(index)
            .service(next_reminders)
            .service(events)
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
