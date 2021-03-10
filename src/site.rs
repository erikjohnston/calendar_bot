//! The web site for the app.

use actix_web::{
    cookie::{Cookie, SameSite},
    error::{ErrorForbidden, ErrorInternalServerError, ErrorNotFound},
    get,
    middleware::Logger,
    post,
    web::{Data, Form, Path, Query},
    HttpResponse, HttpServer, Responder,
};
use anyhow::Error;
use chrono::{Duration, Utc};
use itertools::Itertools;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing_actix_web::TracingLogger;

use crate::app::App;
use crate::auth::AuthedUser;
use crate::database::Reminder;

#[get("/")]
async fn index(_: AuthedUser) -> impl Responder {
    let mut builder = HttpResponse::SeeOther();
    builder.insert_header(("Location", "/calendars"));
    builder.finish()
}

async fn assert_user_owns_calendar(
    app: &App,
    auth_user: AuthedUser,
    calendar_id: i64,
) -> Result<(), actix_web::Error> {
    let calendar = app
        .database
        .get_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    match calendar {
        Some(cal) if cal.user_id == *auth_user => Ok(()),
        _ => Err(ErrorForbidden("forbidden")),
    }
}

async fn assert_user_can_edit_reminder(
    app: &App,
    auth_user: AuthedUser,
    reminder_id: i64,
) -> Result<(), actix_web::Error> {
    // We check by pulling out all reminders the user can see for the event.

    let reminders = app
        .database
        .get_users_who_can_edit_reminder(reminder_id)
        .await
        .map_err(ErrorInternalServerError)?;

    if reminders.contains(&*auth_user) {
        Ok(())
    } else {
        Err(ErrorForbidden("forbidden"))
    }
}

/// List all events in a calendar
#[get("/events/{calendar_id}")]
async fn list_events_calendar_html(
    app: Data<App>,
    path: Path<(i64,)>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();

    assert_user_owns_calendar(&app, user, calendar_id).await?;

    let events = app
        .database
        .get_events_in_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "events": events.iter().map(|(event, instances)| {
            json!({
                "event_id": &event.event_id,
                "calendar_id": &event.calendar_id,
                "summary": &event.summary,
                "description": &event.description,
                "location": &event.location,
                "next_dates": instances.iter().map(|i| i.date.to_string()).collect_vec(),
            })
        }).collect_vec(),
        "calendar_id": calendar_id,
    });

    let result = app
        .templates
        .render(
            "events.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[get("/events")]
async fn list_events_html(
    app: Data<App>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let events = app
        .database
        .get_events_for_user(*user)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "events": events.iter().map(|(event, instances)| {
            json!({
                "event_id": &event.event_id,
                "calendar_id": &event.calendar_id,
                "summary": &event.summary,
                "description": &event.description,
                "location": &event.location,
                "next_dates": instances.iter().map(|i| i.date.to_string()).collect_vec(),
            })
        }).collect_vec(),
    });

    let result = app
        .templates
        .render(
            "events.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[get("/calendars")]
async fn list_calendars_html(
    app: Data<App>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let calendars = app
        .database
        .get_calendars_for_user(*user)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "calendars": calendars,
    });

    let result = app
        .templates
        .render(
            "calendars.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[derive(Debug, Clone, Deserialize)]
struct EventFormState {
    state: Option<String>,
}

#[get("/event/{calendar_id}/{event_id}/new_reminder")]
async fn new_reminder_html(
    app: Data<App>,
    path: Path<(i64, String)>,
    query: Query<EventFormState>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    assert_user_owns_calendar(&app, user, calendar_id).await?;

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

    let context = json!({
        "event": {
            "event_id": &event.event_id,
            "summary": &event.summary,
            "description": &event.description,
            "location": &event.location,
            "next_dates": instances.iter().map(|i| i.date.to_string()).collect_vec()
        },
        "calendar_id": calendar_id,
        "default_template": crate::DEFAULT_TEMPLATE,
        "form_state": state,
    });

    let result = app
        .templates
        .render(
            "reminder.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[get("/event/{calendar_id}/{event_id}/reminder/{reminder_id}")]
async fn get_reminder_html(
    app: Data<App>,
    path: Path<(i64, String, i64)>,
    query: Query<EventFormState>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id, reminder_id) = path.into_inner();

    assert_user_can_edit_reminder(&app, user, reminder_id).await?;

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
        .get_reminder_in_calendar(calendar_id, reminder_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let reminder = if let Some(reminder) = reminder {
        reminder
    } else {
        return Err(actix_web::error::ErrorNotFound("Couldn't find reminder"));
    };

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

    let result = app
        .templates
        .render(
            "reminder.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[get("/event/{calendar_id}/{event_id}")]
async fn get_event_html(
    app: Data<App>,
    path: Path<(i64, String)>,
    query: Query<EventFormState>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    assert_user_owns_calendar(&app, user, calendar_id).await?;

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

    let reminders = app
        .database
        .get_reminders_for_event(calendar_id, &event_id)
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
        "reminders": reminders,
        "default_template": crate::DEFAULT_TEMPLATE,
        "form_state": state,
    });

    let result = app
        .templates
        .render(
            "event.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[post("/event/{calendar_id}/{event_id}/delete_reminder")]
async fn delete_reminder_html(
    app: Data<App>,
    path: Path<(i64, String)>,
    data: Form<UpdateReminderForm>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    let reminder_id = if let Some(reminder_id) = data.reminder_id {
        reminder_id
    } else {
        return Err(actix_web::error::ErrorNotFound("Couldn't find reminder"));
    };

    assert_user_can_edit_reminder(&app, user, reminder_id).await?;

    app.database
        .delete_reminder_in_calendar(calendar_id, reminder_id)
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
    pub reminder_id: Option<i64>,
    pub use_default: Option<String>, // A checkbox, so `Some()` if checked, `None` if not.
    pub template: Option<String>,
    pub minutes_before: i64,
    pub room_id: String,
    pub attendee_editable: Option<String>, // A checkbox, so `Some()` if checked, `None` if not.
}

#[post("/event/{calendar_id}/{event_id}/reminder")]
async fn upsert_reminder_html(
    app: Data<App>,
    path: Path<(i64, String)>,
    data: Form<UpdateReminderForm>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id, event_id) = path.into_inner();

    let data = data.into_inner();

    let template = if data.use_default.is_some() {
        None
    } else {
        data.template.as_deref()
    };

    if let Some(reminder_id) = data.reminder_id {
        assert_user_can_edit_reminder(&app, user, reminder_id).await?;

        app.database
            .update_reminder(
                calendar_id,
                reminder_id,
                &data.room_id,
                data.minutes_before,
                template,
                data.attendee_editable.is_some(),
            )
            .await
            .map_err(ErrorInternalServerError)?;
    } else {
        assert_user_owns_calendar(&app, user, calendar_id).await?;

        app.database
            .add_reminder(Reminder {
                reminder_id: -1, // We're inserting so we use a fake ID
                user_id: *user,
                calendar_id,
                event_id: event_id.clone(),
                room_id: data.room_id,
                minutes_before: data.minutes_before,
                template: template.map(ToOwned::to_owned),
                attendee_editable: data.attendee_editable.is_some(),
            })
            .await
            .map_err(ErrorInternalServerError)?;
    }

    app.update_reminders()
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

#[get("/calendar/{calendar_id}")]
async fn get_calendar_html(
    app: Data<App>,
    path: Path<(i64,)>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();
    assert_user_owns_calendar(&app, user, calendar_id).await?;

    let calendar = app
        .database
        .get_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "calendar": calendar,
    });

    let result = app
        .templates
        .render(
            "calendar.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[get("/calendar/new")]
async fn new_calendar_html(
    app: Data<App>,
    _user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let context = json!({});

    let result = app
        .templates
        .render(
            "calendar.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateCalendarForm {
    pub name: String,
    pub url: String,
    pub user_name: Option<String>,
    pub password: Option<String>,
}

#[post("/calendar/{calendar_id}/edit")]
async fn edit_calendar_html(
    app: Data<App>,
    path: Path<(i64,)>,
    data: Form<UpdateCalendarForm>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();

    assert_user_owns_calendar(&app, user, calendar_id).await?;

    let existing_calendar = app
        .database
        .get_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?
        .ok_or_else(|| ErrorNotFound("No such calendar"))?;

    let UpdateCalendarForm {
        name,
        url,
        mut user_name,
        mut password,
    } = data.into_inner();

    if user_name.as_deref() == Some("") {
        user_name = None;
    }
    if password.as_deref() == Some("") {
        password = None;
    }

    // Awful hack to keep password unchanged if left blank, but still using
    // basic auth.
    if password.is_none() && user_name.is_some() {
        password = existing_calendar.password.clone();
    }

    app.database
        .update_calendar(calendar_id, name, url, user_name, password)
        .await
        .map_err(ErrorInternalServerError)?;

    let new_calendar = app
        .database
        .get_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?
        .ok_or_else(|| ErrorNotFound("No such calendar"))?;

    app.update_calendar(new_calendar)
        .await
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::SeeOther();
    builder.insert_header(("Location", format!("/calendar/{}?state=saved", calendar_id)));
    let response = builder.finish();

    Ok(response)
}

#[post("/calendar/{calendar_id}/delete")]
async fn delete_calendar_html(
    app: Data<App>,
    path: Path<(i64,)>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let (calendar_id,) = path.into_inner();

    assert_user_owns_calendar(&app, user, calendar_id).await?;

    app.database
        .delete_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::SeeOther();
    builder.insert_header(("Location", "/calendars"));
    let response = builder.finish();

    Ok(response)
}

#[post("/calendar/new")]
async fn add_new_calendar_html(
    app: Data<App>,
    data: Form<UpdateCalendarForm>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let UpdateCalendarForm {
        name,
        url,
        mut user_name,
        mut password,
    } = data.into_inner();

    if user_name.as_deref() == Some("") {
        user_name = None;
    }
    if password.as_deref() == Some("") {
        password = None;
    }

    let calendar_id = app
        .database
        .add_calendar(*user, name, url, user_name, password)
        .await
        .map_err(ErrorInternalServerError)?;

    let new_calendar = app
        .database
        .get_calendar(calendar_id)
        .await
        .map_err(ErrorInternalServerError)?
        .ok_or_else(|| ErrorNotFound("No such calendar"))?;

    app.update_calendar(new_calendar)
        .await
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::SeeOther();
    builder.insert_header(("Location", format!("/calendar/{}?state=saved", calendar_id)));
    let response = builder.finish();

    Ok(response)
}

#[get("/login")]
async fn login_get_html(app: Data<App>) -> Result<impl Responder, actix_web::Error> {
    let context = json!({});

    let result = app
        .templates
        .render(
            "login.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

#[derive(Debug, Deserialize, Clone)]
struct LoginForm {
    user_name: String,
    password: String,
}

#[post("/login")]
async fn login_post_html(
    app: Data<App>,
    data: Form<LoginForm>,
) -> Result<impl Responder, actix_web::Error> {
    let user_id = app
        .database
        .check_password(&data.user_name, &data.password)
        .await
        .map_err(ErrorInternalServerError)?;

    let response = if let Some(user_id) = user_id {
        let token: String = rand::thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();

        app.database
            .add_access_token(user_id, &token, Utc::now() + Duration::days(7))
            .await
            .map_err(ErrorInternalServerError)?;

        let cookie = Cookie::build("token", token)
            .same_site(SameSite::Strict)
            .max_age(time::Duration::days(7))
            .http_only(true)
            .finish();

        HttpResponse::SeeOther()
            .insert_header(("Location", "/calendars"))
            .cookie(cookie)
            .finish()
    } else {
        HttpResponse::SeeOther()
            .insert_header(("Location", "/login?state=invalid_password"))
            .finish()
    };

    Ok(response)
}

/// Run the HTTP server.
pub async fn run_server(app: App) -> Result<(), Error> {
    let bind_addr = app
        .config
        .app
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
            .service(list_events_html)
            .service(list_events_calendar_html)
            .service(new_reminder_html)
            .service(get_reminder_html)
            .service(get_event_html)
            .service(delete_reminder_html)
            .service(upsert_reminder_html)
            .service(list_calendars_html)
            .service(new_calendar_html)
            .service(add_new_calendar_html)
            .service(get_calendar_html)
            .service(edit_calendar_html)
            .service(delete_calendar_html)
            .service(login_get_html)
            .service(login_post_html)
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
