//! Helper functions for parsing and dealing with ICS calendars.

use anyhow::{anyhow, bail, Context, Error};
use chrono::{Duration, Utc};
use ics_parser::{
    components::{VCalendar, VEvent},
    parser,
    property::PropertyValue,
};
use reqwest::Method;
use sentry::integrations::anyhow::capture_anyhow;
use tracing::{error, info, instrument, Span};
use url::Url;

use std::{convert::TryInto, ops::Deref, str::FromStr};

use crate::database::{Attendee, CalendarAuthentication, Event, EventInstance};

/// Parse a ICS encoded calendar.
fn decode_calendar(cal_body: &str) -> Result<Vec<VCalendar>, Error> {
    let components =
        parser::Component::from_str_to_stream(cal_body).with_context(|| "decoding component")?;

    components
        .into_iter()
        .map(|comp| comp.try_into().with_context(|| "decoding VCALENDAR"))
        .collect()
}

/// Fetch a calendar from a CalDAV URL and parse the returned set of calendars.
///
/// Note that CalDAV returns a calendar per event, rather than one calendar with
/// many events.
#[instrument(skip(client), fields(status))]
pub async fn fetch_calendars(
    client: &reqwest::Client,
    url: &str,
    authentication: &CalendarAuthentication,
) -> Result<Vec<VCalendar>, Error> {
    let mut req = client
        .request(Method::from_str("REPORT").expect("method"), url)
        .header("Content-Type", "application/xml");

    match authentication {
        CalendarAuthentication::None => {}
        CalendarAuthentication::Basic {
            user_name,
            password,
        } => req = req.basic_auth(user_name, Some(password)),
        CalendarAuthentication::Bearer { access_token } => req = req.bearer_auth(access_token),
    }

    // We fetch all calendar events from the previous N months and following, to
    // try and mitigate a bug where the returned calendar doesn't include a base
    // event for a recurring override.
    let start = Utc::now() - Duration::days(30) * 6;

    let resp = req
        .body(format!(
            r#"
        <c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
            <d:prop>
                <d:getetag />
                <c:calendar-data />
            </d:prop>
            <c:filter>
                <c:comp-filter name="VCALENDAR">
                    <c:comp-filter name="VEVENT" >
                    <c:time-range start="{start}" />
                    </c:comp-filter>
                </c:comp-filter>
            </c:filter>
        </c:calendar-query>
        "#,
            start = start.format("%Y%m%dT%H%M%SZ"),
        ))
        .send()
        .await?;

    let status = resp.status();

    let body = resp.text().await?;

    info!(status = status.as_u16(), "Got result from CalDAV");
    Span::current().record("status", status.as_u16());

    if !status.is_success() {
        bail!("Got {} result from CalDAV", status.as_u16());
    }

    let doc = roxmltree::Document::parse(&body)
        .map_err(|e| anyhow!(e))
        .with_context(|| "decoding xml")?;

    let mut calendars = Vec::new();

    for node in doc.descendants() {
        if node.tag_name().name() != "calendar-data" {
            continue;
        }

        let cal_body = if let Some(t) = node.text() {
            t
        } else {
            continue;
        };

        match decode_calendar(cal_body) {
            Ok(cals) => calendars.extend(cals),
            Err(e) => {
                capture_anyhow(&e);
                error!(
                    error = e.deref() as &dyn std::error::Error,
                    "Failed to parse calendar"
                )
            }
        }
    }

    Ok(calendars)
}

/// Parse the calendars into events and event instances.
pub fn parse_calendars_to_events(
    calendar_id: i64,
    calendars: &[VCalendar],
) -> Result<(Vec<Event>, Vec<EventInstance>), Error> {
    let now = Utc::now();
    let mut events: Vec<Event> = Vec::new();
    let mut next_dates = Vec::new();
    for calendar in calendars {
        for (uid, event) in &calendar.events {
            if event.base_event.is_full_day_event() || event.base_event.is_floating_event() {
                continue;
            }

            let mut organizer = None;
            for prop in &event.base_event.properties {
                if let ics_parser::property::Property::Organizer(prop) = prop {
                    organizer = parse_to_attendee(prop);
                }
            }

            events.push(Event {
                calendar_id,
                event_id: uid.clone(),
                summary: event.base_event.summary.clone(),
                description: event.base_event.description.clone(),
                location: event.base_event.location.clone(),
                organizer,
                attendees: get_attendees(&event.base_event),
            });

            // Loop through all occurrences of the event in the next N days and
            // generate `EventInstance` for them.
            for (date, recur_event) in event
                .recur_iter(calendar)?
                .skip_while(|(d, _)| *d < now - Duration::days(7))
                .take_while(|(d, _)| *d < now + Duration::days(30))
            {
                // Loop over all the properties to pull out the attendee info.

                next_dates.push(EventInstance {
                    event_id: uid.into(),
                    date,
                    attendees: get_attendees(recur_event),
                });
            }
        }
    }
    Ok((events, next_dates))
}

/// Parse the attendees from the event.
fn get_attendees(event: &VEvent) -> Vec<Attendee> {
    let mut attendees = Vec::new();

    for prop in &event.properties {
        if let ics_parser::property::Property::Attendee(prop) = prop {
            if let Some(attendee) = parse_to_attendee(prop) {
                attendees.push(attendee)
            }
        }
    }

    attendees
}

/// Parse a attendee property.
fn parse_to_attendee(prop: &PropertyValue<Url>) -> Option<Attendee> {
    if prop.value.scheme() != "mailto" {
        return None;
    }

    let email = prop.value.path().to_string();

    let mut common_name = None;
    for param in prop.parameters.parameters() {
        match param {
            ics_parser::parameters::Parameter::CN(cn) => {
                common_name = Some(cn.clone());
            }
            ics_parser::parameters::Parameter::ParticipationStatus(status)
                if status == "DECLINED" =>
            {
                return None
            }
            _ => {}
        }
    }

    Some(Attendee { email, common_name })
}
