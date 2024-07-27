use actix_web::test::read_body;
use anyhow::{Context, Error};

pub mod common;

use calendar_bot::site::UpdateCalendarForm;
use common::{create_actix_app, create_user_and_login, Form};
use httptest::{matchers::request, responders::status_code};
use scraper::{Html, Selector};
use tracing::error;

const BODY: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><response><href>/test.ics</href><propstat><prop><getetag>"c72d910d9c3315e5e584c4e6bde47e2dfeecad4d45eaf434ce0ab54065fdb7b3"</getetag>
<C:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Mozilla.org/NONSGML Mozilla Calendar V1.1//EN
BEGIN:VTIMEZONE
TZID:Europe/London
BEGIN:STANDARD
DTSTART:19701025T020000
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10
TZNAME:GMT
TZOFFSETFROM:+0100
TZOFFSETTO:+0000
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:19700329T010000
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3
TZNAME:BST
TZOFFSETFROM:+0000
TZOFFSETTO:+0100
END:DAYLIGHT
END:VTIMEZONE
BEGIN:VEVENT
UID:d8991eee-41eb-404d-a37c-0717ba3b4f74
DTSTART;TZID=Europe/London:20211124T100000
DTEND;TZID=Europe/London:20211124T100500
ATTENDEE;CN=Tester:mailto:test@example.com
ORGANIZER:mailto:test@example.com
CREATED:20211126T094315Z
DTSTAMP:20220425T104310Z
LAST-MODIFIED:20220425T104310Z
RRULE:FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR
SUMMARY:Test Event
TRANSP:TRANSPARENT
X-MOZ-GENERATION:4
END:VEVENT
END:VCALENDAR
</C:calendar-data></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>
"#;

/// Test we can add a calendar via HTML
#[test_log::test(actix_web::test)]
async fn test_add_noauth_calendar() -> Result<(), Error> {
    let (app, _db, actix_app) = create_actix_app().await?;

    let cookie = create_user_and_login(&app, "bob").await?;

    let req = actix_web::test::TestRequest::get()
        .uri("/calendar/new")
        .cookie(cookie.clone())
        .to_request();
    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_success(), "status: {}", resp.status());

    // Check that body is valid html
    let bytes = read_body(resp).await;
    let document = Html::parse_document(std::str::from_utf8(&bytes)?);
    assert_html!(document);

    // Set up a CALDAV test server
    let mut caldav_server = httptest::Server::run();
    caldav_server.expect(
        httptest::Expectation::matching(request::method_path("REPORT", "/calendar"))
            .respond_with(status_code(200).body(BODY)),
    );

    // Create a new calendar form, verify it against the HTML form and send it.
    let calendar_form = UpdateCalendarForm {
        name: "test calendar".to_string(),
        url: caldav_server.url("/calendar").to_string(),
        user_name: None,
        password: None,
    };

    let form = Form::from_html(document)?;
    let form_request = form.to_request(&calendar_form)?;

    let resp =
        actix_web::test::call_service(&actix_app, form_request.cookie(cookie.clone()).to_request())
            .await;

    // We get redirected to the calendar edit pages.
    assert!(resp.status().is_redirection(), "status: {}", resp.status());
    let location = resp.headers().get("location").context("location header")?;

    let req = actix_web::test::TestRequest::get()
        .uri(location.to_str()?)
        .cookie(cookie.clone())
        .to_request();
    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_success(), "status: {}", resp.status());

    assert_html_response!(resp);

    // We should have seen a request to the caldav server by now.
    caldav_server.verify_and_clear();

    // Check that the /events page includes the new event
    let req = actix_web::test::TestRequest::get()
        .uri("/events")
        .cookie(cookie.clone())
        .to_request();
    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_success(), "status: {}", resp.status());

    let bytes = read_body(resp).await;
    let document = Html::parse_document(std::str::from_utf8(&bytes)?);
    assert_html!(document);

    let summary_selector = Selector::parse("h3 > a").unwrap();
    if !document
        .select(&summary_selector)
        .any(|element| element.inner_html() == "Test Event")
    {
        panic!("Could not find test event!");
    }

    Ok(())
}
