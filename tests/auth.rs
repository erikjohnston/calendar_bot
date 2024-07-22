use actix_web::cookie::Cookie;
use anyhow::{Context, Error};
use serde_json::json;

pub mod common;

use common::create_actix_app;

/// Test logging in with username and password works.
#[test_log::test(actix_web::test)]
async fn test_password_login() -> Result<(), Error> {
    let (app, _db, actix_app) = create_actix_app().await?;

    let req = actix_web::test::TestRequest::get()
        .uri("/login")
        .to_request();
    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_success(), "status: {}", resp.status());

    // Try and login with the wrong user
    let req = actix_web::test::TestRequest::post()
        .uri("/login")
        .set_form(json!({"user_name": "bob", "password": "pass"}))
        .to_request();

    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_redirection(), "status: {}", resp.status());
    let location = resp.headers().get("location").context("location header")?;
    assert_eq!(location.to_str()?, "/login?state=invalid_password");

    // Create a user and password
    let user_id: i64 = app.database.upsert_account("bob").await?;
    app.database.change_password(user_id, "pass").await?;

    // Login with correct user/password
    let req = actix_web::test::TestRequest::post()
        .uri("/login")
        .set_form(json!({"user_name": "bob", "password": "pass"}))
        .to_request();

    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_redirection(), "status: {}", resp.status());
    let location = resp.headers().get("location").context("location header")?;
    assert_eq!(location.to_str()?, "/calendars");

    // Check we have cookie for the token
    let cookie = resp.headers().get("set-cookie").context("cookie")?;

    let cookie = Cookie::parse(cookie.to_str()?).unwrap();
    assert_eq!(cookie.name(), "token");
    assert_eq!(cookie.http_only(), Some(true));
    assert!(cookie.max_age().is_some());

    // Check that fetching /calendars works with the token
    let req = actix_web::test::TestRequest::get()
        .uri("/calendars")
        .cookie(cookie)
        .to_request();
    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_success(), "status: {}", resp.status());

    Ok(())
}
