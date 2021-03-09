use std::{fmt::Display, ops::Deref, pin::Pin};

use actix_web::{
    error::ErrorInternalServerError, web::Data, Error, FromRequest, HttpMessage, HttpResponse,
    ResponseError,
};
use futures::{Future, FutureExt};

use crate::app::App;

/// Extractor that gets the authenticated user.
#[derive(Debug, Clone, Copy)]
pub struct AuthedUser(pub i64);

impl Deref for AuthedUser {
    type Target = i64;

    fn deref(&self) -> &i64 {
        &self.0
    }
}

impl FromRequest for AuthedUser {
    type Config = ();

    type Error = Error;

    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        _payload: &mut actix_web::dev::Payload,
    ) -> Self::Future {
        let app = req.app_data::<Data<App>>().expect("no app").deref().clone();
        let req = req.clone();

        async move {
            let cookie = req.cookie("token").ok_or(NotAuthedError)?;

            let token = cookie.value();

            let user_id_opt = app
                .database
                .get_user_from_token(token)
                .await
                .map_err(ErrorInternalServerError)?;

            let user_id = user_id_opt.ok_or(NotAuthedError)?;

            Ok(AuthedUser(user_id))
        }
        .boxed_local()
    }
}

#[derive(Debug, Clone)]
pub struct NotAuthedError;

impl Display for NotAuthedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Not logged in")
    }
}

impl ResponseError for NotAuthedError {
    fn status_code(&self) -> reqwest::StatusCode {
        reqwest::StatusCode::SEE_OTHER
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code())
            .insert_header(("Location", "/login"))
            .finish()
    }
}
