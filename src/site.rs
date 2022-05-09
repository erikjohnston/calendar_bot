//! The web site for the app.

use actix_web::{
    cookie::{Cookie, SameSite},
    error::{ErrorBadRequest, ErrorForbidden, ErrorInternalServerError, ErrorNotFound},
    get,
    middleware::Logger,
    post,
    web::{Data, Form, Path, Query},
    HttpResponse, HttpServer, Responder,
};
use anyhow::Error;

use itertools::Itertools;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing_actix_web::TracingLogger;

use crate::database::Reminder;
use crate::{app::TryAuthenticatedAPI, auth::AuthedUser};
use crate::{
    app::{is_likely_a_valid_user_id, App},
    database::CalendarAuthentication,
};

/// Root handler.
#[get("/")]
async fn index(_: AuthedUser) -> impl Responder {
    let mut builder = HttpResponse::SeeOther();
    builder.insert_header(("Location", "/calendars"));
    builder.finish()
}

/// Asserts that the user owns the calendar
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

/// Asserts that the user can edit the reminder
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

    let email = app
        .database
        .get_email(user.0)
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
                "next_dates": instances.iter().map(|i| i.date.to_rfc3339()).collect_vec(),
            })
        }).collect_vec(),
        "calendar_id": calendar_id,
        "email": email,
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

/// List all events in all calendars for the user.
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

    let email = app
        .database
        .get_email(user.0)
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
                "next_dates": instances.iter().map(|i| i.date.to_rfc3339()).collect_vec(),
            })
        }).collect_vec(),
        "email": email,
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

/// List all reminders owned by the user.
#[get("/reminders")]
async fn list_events_wit_reminders_html(
    app: Data<App>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let events = app
        .database
        .get_events_with_reminders(*user)
        .await
        .map_err(ErrorInternalServerError)?;

    let email = app
        .database
        .get_email(user.0)
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
                "next_dates": instances.iter().map(|i| i.date.to_rfc3339()).collect_vec(),
            })
        }).collect_vec(),
        "email": email,
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

/// List all calendars for the user.
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

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "calendars": calendars,
        "email": email,
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

/// Used to parse url that may have a `state` query param.
#[derive(Debug, Clone, Deserialize)]
struct EventFormState {
    state: Option<String>,
}

/// Create a new reminder
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

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "event": {
            "event_id": &event.event_id,
            "summary": &event.summary,
            "description": &event.description,
            "location": &event.location,
            "next_dates": instances.iter().map(|i| i.date.to_rfc3339()).collect_vec()
        },
        "calendar_id": calendar_id,
        "default_template": crate::DEFAULT_TEMPLATE,
        "form_state": state,
        "email": email,
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

/// Get an existing reminder
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

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "event": {
            "event_id": &event.event_id,
            "summary": &event.summary,
            "description": &event.description,
            "location": &event.location,
            "next_dates": instances.iter().map(|i| i.date.to_rfc3339()).collect_vec()
        },
        "calendar_id": calendar_id,
        "reminder": reminder,
        "default_template": crate::DEFAULT_TEMPLATE,
        "form_state": state,
        "email": email,
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

/// Get an event.
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

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "event": {
            "event_id": &event.event_id,
            "summary": &event.summary,
            "description": &event.description,
            "location": &event.location,
            "next_dates": instances.iter().map(|i| i.date.to_rfc3339()).collect_vec()
        },
        "calendar_id": calendar_id,
        "reminders": reminders,
        "default_template": crate::DEFAULT_TEMPLATE,
        "form_state": state,
        "email": email,
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

/// Delete a reminder
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

/// Form body for updating/adding a reminder
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateReminderForm {
    pub reminder_id: Option<i64>,
    pub use_default: Option<String>, // A checkbox, so `Some()` if checked, `None` if not.
    pub template: Option<String>,
    pub minutes_before: i64,
    pub room: String,
    pub attendee_editable: Option<String>, // A checkbox, so `Some()` if checked, `None` if not.
}

/// Add or update a reminder.
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
                &data.room,
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
                room: data.room,
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

/// Get calendar info
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

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "calendar": calendar,
        "email": email,
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

/// Add new calendar
#[get("/calendar/new")]
async fn new_calendar_html(
    app: Data<App>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({ "email": email });

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

/// Form body for editing a calendar's config
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateCalendarForm {
    pub name: String,
    pub url: String,
    pub user_name: Option<String>,
    pub password: Option<String>,
}

/// Edit a calendar's config.
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
        let existing_password = match existing_calendar.authentication {
            CalendarAuthentication::Basic { ref password, .. } => password.clone(),
            _ => return Err(ErrorInternalServerError("Calendar doesn't have a password")),
        };
        password = Some(existing_password)
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

/// Delete a calendar
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

/// Add a new calendar page.
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

/// Login page
#[get("/login")]
async fn login_get_html(app: Data<App>) -> Result<impl Responder, actix_web::Error> {
    let sso_name = app.config.sso.as_ref().map(|s| &s.display_name);
    let context = json!({ "sso_name": sso_name });

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

/// Form body for logging in.
#[derive(Debug, Deserialize, Clone)]
struct LoginForm {
    user_name: String,
    password: String,
}

/// Login
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
        let token = app
            .add_access_token(user_id)
            .await
            .map_err(ErrorInternalServerError)?;

        let cookie = Cookie::build("token", token)
            .same_site(SameSite::Lax)
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

/// Change password page
#[get("/change_password")]
async fn change_password_html(
    app: Data<App>,
    user: AuthedUser,
    query: Query<EventFormState>,
) -> Result<impl Responder, actix_web::Error> {
    let state = match query.into_inner().state.as_deref() {
        Some("saved") => Some("saved"),
        Some("wrong_password") => Some("wrong_password"),
        Some("password_mismatch") => Some("password_mismatch"),
        _ => None,
    };

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "form_state": state,
        "email": email,
    });

    let result = app
        .templates
        .render(
            "change_password.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

/// Form body for changing password
#[derive(Debug, Deserialize, Clone)]
struct ChangePasswordForm {
    old_password: String,
    new_password: String,
    confirm_password: String,
}

/// Change password
#[post("/change_password")]
async fn change_password_post_html(
    app: Data<App>,
    data: Form<ChangePasswordForm>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    if data.new_password != data.confirm_password {
        return Ok(HttpResponse::SeeOther()
            .insert_header(("Location", "/change_password?state=password_mismatch"))
            .finish());
    }

    let right_password = app
        .database
        .check_password_user_id(user.0, &data.old_password)
        .await
        .map_err(ErrorInternalServerError)?;

    let response = if right_password.is_some() {
        app.database
            .change_password(user.0, &data.new_password)
            .await
            .map_err(ErrorInternalServerError)?;

        HttpResponse::SeeOther()
            .insert_header(("Location", "/change_password?state=saved"))
            .finish()
    } else {
        HttpResponse::SeeOther()
            .insert_header(("Location", "/change_password?state=wrong_password"))
            .finish()
    };

    Ok(response)
}

/// Change Matrix ID page
#[get("/change_matrix_id")]
async fn change_matrix_id_html(
    app: Data<App>,
    user: AuthedUser,
    query: Query<EventFormState>,
) -> Result<impl Responder, actix_web::Error> {
    let state = query.into_inner().state;

    let old_matrix_id = app
        .database
        .get_matrix_id(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    let context = json!({
        "form_state": state,
        "old_matrix_id": old_matrix_id,
        "email": email,
    });

    let result = app
        .templates
        .render(
            "change_matrix_id.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

/// Change Matrix ID page
#[get("/google_calendars")]
async fn google_calendars(
    app: Data<App>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    let calendars = match app
        .get_google_calendars("/google_calendars", user.0)
        .await
        .map_err(ErrorInternalServerError)?
    {
        TryAuthenticatedAPI::Success(calendars) => calendars,
        TryAuthenticatedAPI::Redirect(url) => {
            return Ok(HttpResponse::SeeOther()
                .insert_header(("Location", url.to_string()))
                .finish())
        }
    };

    let context = json!({
        "calendars": calendars,
    });

    let result = app
        .templates
        .render(
            "list_google_calendars.html.j2",
            &tera::Context::from_serialize(&context).map_err(ErrorInternalServerError)?,
        )
        .map_err(ErrorInternalServerError)?;

    let mut builder = HttpResponse::Ok();
    builder.insert_header(("Content-Type", "text/html; charset=utf-8"));
    let response = builder.body(result);

    Ok(response)
}

/// Form body for changing password
#[derive(Debug, Deserialize, Clone)]
struct ChangeMatrixIdForm {
    new_matrix_id: String,
}

/// Change Matrix ID
#[post("/change_matrix_id")]
async fn change_matrix_id_post_html(
    app: Data<App>,
    data: Form<ChangeMatrixIdForm>,
    user: AuthedUser,
) -> Result<impl Responder, actix_web::Error> {
    if !is_likely_a_valid_user_id(&data.new_matrix_id) {
        return Err(ErrorBadRequest("That does not look like a Matrix ID."));
    }

    let email = app
        .database
        .get_email(user.0)
        .await
        .map_err(ErrorInternalServerError)?;

    app.database
        .replace_matrix_id(&email, &data.new_matrix_id)
        .await
        .map_err(ErrorInternalServerError)?;

    Ok(HttpResponse::SeeOther()
        .insert_header(("Location", "/change_matrix_id?state=saved"))
        .finish())
}

/// Redirect to SSO for login, if configured.
#[get("/sso_redirect")]
async fn sso_redirect(app: Data<App>) -> Result<impl Responder, actix_web::Error> {
    let auth_url = app
        .start_login_via_sso()
        .await
        .map_err(ErrorInternalServerError)?;

    let response = HttpResponse::TemporaryRedirect()
        .insert_header(("Location", auth_url.to_string()))
        .finish();

    Ok(response)
}

/// The `state` query param for SSO requests.
#[derive(Debug, Deserialize, Clone)]
struct SsoStateParam {
    state: String,
    code: String,
}

/// Finish SSO auth.
#[get("/sso_callback")]
async fn sso_auth(
    app: Data<App>,
    query: Query<SsoStateParam>,
) -> Result<impl Responder, actix_web::Error> {
    let email = app
        .finish_login_via_sso(query.state.clone(), query.code.clone())
        .await
        .map_err(ErrorInternalServerError)?;

    let user_id = app
        .database
        .upsert_account(&email)
        .await
        .map_err(ErrorInternalServerError)?;

    let token = app
        .add_access_token(user_id)
        .await
        .map_err(ErrorInternalServerError)?;

    let cookie = Cookie::build("token", token)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(7))
        .http_only(true)
        .finish();

    Ok(HttpResponse::SeeOther()
        .insert_header(("Location", "/calendars"))
        .cookie(cookie)
        .finish())
}

/// Finish OAuth2 flow.
#[get("/oauth2/callback")]
async fn oauth2_callback(
    app: Data<App>,
    query: Query<SsoStateParam>,
) -> Result<impl Responder, actix_web::Error> {
    let path = app
        .finish_google_oauth_session(&query.state, query.code.clone())
        .await
        .map_err(ErrorInternalServerError)?;

    let response = HttpResponse::TemporaryRedirect()
        .insert_header(("Location", path))
        .finish();

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
            .app_data(Data::new(app.clone()))
            .wrap(TracingLogger::default())
            .wrap(Logger::default())
            .service(index)
            .service(list_events_html)
            .service(list_events_wit_reminders_html)
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
            .service(change_password_html)
            .service(change_password_post_html)
            .service(change_matrix_id_html)
            .service(change_matrix_id_post_html)
            .service(sso_redirect)
            .service(sso_auth)
            .service(oauth2_callback)
            .service(google_calendars)
    })
    .bind(&bind_addr)?
    .run()
    .await?;

    Ok(())
}
