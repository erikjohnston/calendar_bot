use anyhow::Error;

mod common;

use common::{create_actix_app, create_user_and_login};

/// Test that starting the app works, and `/` responds with something
#[test_log::test(actix_web::test)]
async fn test_start_app() -> Result<(), Error> {
    let (_app, _db, actix_app) = create_actix_app().await?;

    let req = actix_web::test::TestRequest::get().uri("/").to_request();
    let resp = actix_web::test::call_service(&actix_app, req).await;
    assert!(resp.status().is_redirection(), "status: {}", resp.status());

    Ok(())
}

/// Test that calling simple endpoints work with a blank account
#[test_log::test(actix_web::test)]
async fn test_endpoints() -> Result<(), Error> {
    let (app, _db, actix_app) = create_actix_app().await?;

    let cookie = create_user_and_login(&app, "bob").await?;

    for path in &[
        "/events",
        "/reminders",
        "/calendars",
        "/calendar/new",
        "/change_password",
        "/change_matrix_id",
    ] {
        let req = actix_web::test::TestRequest::get()
            .uri(path)
            .cookie(cookie.clone())
            .to_request();
        let resp = actix_web::test::call_service(&actix_app, req).await;
        assert!(resp.status().is_success(), "status: {}", resp.status());
    }

    Ok(())
}
