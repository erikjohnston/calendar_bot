use std::collections::HashSet;

use actix_http::Request;
use actix_web::{
    body::MessageBody,
    cookie::Cookie,
    dev::{Service, ServiceResponse},
    middleware::Logger,
};
use anyhow::{bail, Context, Error};
use calendar_bot::config::Config;
use pgtemp::PgTempDB;
use scraper::Selector;
use serde::Serialize;
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

#[macro_export]
macro_rules! assert_html {
    ($document:expr) => {
        for error in &$document.errors {
            error!(error = error.as_ref(), "HTML parsing error");
        }
        assert!($document.errors.is_empty());
    };
}

#[macro_export]
macro_rules! assert_html_response {
    ($resp:expr) => {
        let bytes = read_body($resp).await;
        let document = Html::parse_document(std::str::from_utf8(&bytes)?);
        assert_html!(document);
    };
}

#[derive(Debug, Clone)]
pub struct Form {
    pub path: String,
    pub text_elements: Vec<String>,
}

impl Form {
    pub fn from_html(document: scraper::Html) -> Result<Form, Error> {
        let form_selector = Selector::parse("form").unwrap();
        let input_selector = Selector::parse("input").unwrap();

        let mut form_iter = document.select(&form_selector);
        let form = form_iter.next().context("no form")?;
        assert!(form_iter.next().is_none());

        let mut text_elements = Vec::new();
        let mut path = None;

        for element in form.select(&input_selector) {
            match element.value().attr("type").context("missing type")? {
                "text" | "password" => {
                    let name = element.value().attr("name").context("missing name")?;
                    text_elements.push(name.to_string());
                }
                "submit" => {
                    let formaction = element
                        .value()
                        .attr("formaction")
                        .context("missing formaction")?;
                    path = Some(formaction.to_string());
                }
                t => bail!("unrecognized type '{t}'"),
            }
        }

        let Some(path) = path else {
            bail!("Could not find submission path");
        };

        Ok(Form {
            path,
            text_elements,
        })
    }

    pub fn to_request(&self, data: &impl Serialize) -> Result<actix_web::test::TestRequest, Error> {
        let value = serde_json::to_value(data)?;
        let map = value.as_object().context("get object")?;

        let form_set: HashSet<_> = self.text_elements.iter().collect();
        let data_set: HashSet<_> = map.keys().collect();

        assert_eq!(form_set, data_set);

        let req = actix_web::test::TestRequest::post()
            .uri(&self.path)
            .set_form(data);

        Ok(req)
    }
}
