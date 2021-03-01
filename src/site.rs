//! The web site for the app.

use actix_web::{
    error::ErrorInternalServerError,
    get,
    http::header::ContentType,
    middleware::Logger,
    post,
    web::{Data, Form, Path, Query},
    HttpResponse, HttpServer, Responder,
};
use anyhow::Error;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;
use tracing_actix_web::TracingLogger;

use crate::app::App;

const EVENTS_TEMPLATE: &str = include_str!("res/events.html.j2");
const EVENT_TEMPLATE: &str = include_str!("res/event.html.j2");

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
async fn list_events(
    app: Data<App>,
    path: Path<(i64,)>,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();

    let events = app
        .database
        .get_events_in_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let result = events
        .into_iter()
        .map(|(event, instances)| {
            format!(
                "Summary: {}, Date: {}",
                event.summary.as_deref().unwrap_or("<null>"),
                instances[0].date
            )
        })
        .join("\n\n");

    Ok(result)
}

/// List all events in a calendar
#[get("/admin/event/{calendar_id}/{event_id}")]
async fn get_event(
    app: Data<App>,
    path: Path<(i64, String)>,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    let res = app
        .database
        .get_event_in_calendar(calendar_id, &event_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let (event, instances) = if let Some((event, instances)) = res {
        (event, instances)
    } else {
        return Err(actix_web::error::ErrorNotFound("Couldn't find event"));
    };

    let result = format!(
        "Summary: {}, Dates: {}\n\n{}",
        event.summary.as_deref().unwrap_or("<null>"),
        instances.iter().map(|d| d.date).join(", "),
        event.description.as_deref().unwrap_or(""),
    );

    Ok(result)
}

/// List all events in a calendar
#[get("/events/{calendar_id}")]
async fn list_events_html(
    app: Data<App>,
    path: Path<(i64,)>,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();

    let events = app
        .database
        .get_events_in_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "events": events.iter().map(|(event, instances)| {
            json!({
                "event_id": &event.event_id,
                "summary": &event.summary,
                "description": &event.description,
                "location": &event.location,
                "next_dates": instances.iter().map(|i| i.date.to_string()).collect_vec(),
            })
        }).collect_vec(),
        "calendar_id": calendar_id,
    });

    let result = tera::Tera::one_off(
        EVENTS_TEMPLATE,
        &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        true,
    )
    .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(ContentType::html());
    let response = builder.body(result);

    Ok(response)
}

#[derive(Debug, Clone, Deserialize)]
struct EventFormState {
    state: Option<String>,
}

#[get("/event/{calendar_id}/{event_id}")]
async fn get_event_html(
    app: Data<App>,
    path: Path<(i64, String)>,
    query: Query<EventFormState>,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    let state = match query.into_inner().state.as_deref() {
        Some("saved") => Some("saved"),
        Some("deleted") => Some("deleted"),
        _ => None,
    };

    let res = app
        .database
        .get_event_in_calendar(calendar_id, &event_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let (event, instances) = if let Some((event, instances)) = res {
        (event, instances)
    } else {
        return Err(actix_web::error::ErrorNotFound("Couldn't find event"));
    };

    let reminder = app
        .database
        .get_reminder_for_event(calendar_id, &event_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "event": {
            "event_id": &event.event_id,
            "summary": &event.summary,
            "description": &event.description,
            "location": &event.location,
            "next_dates": instances.iter().map(|i| i.date.to_string()).collect_vec()
        },
        "calendar_id": calendar_id,
        "reminder": reminder,
        "default_template": crate::DEFAULT_TEMPLATE,
        "form_state": state,
    });

    let result = tera::Tera::one_off(
        EVENT_TEMPLATE,
        &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        true,
    )
    .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(ContentType::html());
    let response = builder.body(result);

    Ok(response)
}

#[post("/event/{calendar_id}/{event_id}/delete_reminder")]
async fn delete_reminder_html(
    app: Data<App>,
    path: Path<(i64, String)>,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    app.database
        .delete_reminder(calendar_id, &event_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::SeeOther();
    builder.insert_header((
        "Location",
        format!("/event/{}/{}?state=deleted", calendar_id, event_id),
    ));
    let response = builder.finish();

    Ok(response)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateReminderForm {
    pub use_default: Option<String>, // A checkbox, so `Some()` if checked, `None` if not.
    pub template: Option<String>,
    pub minutes_before: i64,
    pub room_id: String,
}

#[post("/event/{calendar_id}/{event_id}/update_reminder")]
async fn update_reminder_html(
    app: Data<App>,
    path: Path<(i64, String)>,
    data: Form<UpdateReminderForm>,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    let data = data.into_inner();

    let template = if data.use_default.is_some() {
        None
    } else {
        data.template.as_deref()
    };

    app.database
        .upsert_reminder(
            1, // FIXME
            calendar_id,
            &event_id,
            &data.room_id,
            data.minutes_before,
            template,
        )
        .await
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::SeeOther();
    builder.insert_header((
        "Location",
        format!("/event/{}/{}?state=saved", calendar_id, event_id),
    ));
    let response = builder.finish();

    Ok(response)
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
            .service(list_events)
            .service(get_event)
            .service(list_events_html)
            .service(get_event_html)
            .service(delete_reminder_html)
            .service(update_reminder_html)
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
