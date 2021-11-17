//! The high level app.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    error::Error as StdError,
    ops::Deref,
    sync::{Arc, Mutex},
};

use crate::{
    calendar::{fetch_calendars, parse_calendars_to_events},
    config::HiBobConfig,
    database::ReminderInstance,
};
use crate::{config::Config, database::Database};
use crate::{database::Calendar, DEFAULT_TEMPLATE};

use anyhow::{bail, Context, Error};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use comrak::{markdown_to_html, ComrakOptions};
use futures::future;
use handlebars::Handlebars;
use ics_parser::property::EndCondition;
use itertools::Itertools;
use openidconnect::{
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    reqwest::async_http_client,
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use rand::{distributions::Alphanumeric, Rng};
use serde::Deserialize;
use serde_json::json;
use tera::Tera;
use tokio::{
    sync::Notify,
    time::{interval, sleep},
};
use tracing::{error, info, instrument, Span};
use url::Url;
use urlencoding::encode;

/// The type of the OpenID Connect client.
type OpenIDClient = openidconnect::Client<
    openidconnect::EmptyAdditionalClaims,
    openidconnect::core::CoreAuthDisplay,
    openidconnect::core::CoreGenderClaim,
    openidconnect::core::CoreJweContentEncryptionAlgorithm,
    openidconnect::core::CoreJwsSigningAlgorithm,
    openidconnect::core::CoreJsonWebKeyType,
    openidconnect::core::CoreJsonWebKeyUse,
    openidconnect::core::CoreJsonWebKey,
    openidconnect::core::CoreAuthPrompt,
    openidconnect::StandardErrorResponse<openidconnect::core::CoreErrorResponseType>,
    openidconnect::StandardTokenResponse<
        openidconnect::IdTokenFields<
            openidconnect::EmptyAdditionalClaims,
            openidconnect::EmptyExtraTokenFields,
            openidconnect::core::CoreGenderClaim,
            openidconnect::core::CoreJweContentEncryptionAlgorithm,
            openidconnect::core::CoreJwsSigningAlgorithm,
            openidconnect::core::CoreJsonWebKeyType,
        >,
        openidconnect::core::CoreTokenType,
    >,
    openidconnect::core::CoreTokenType,
    openidconnect::StandardTokenIntrospectionResponse<
        openidconnect::EmptyExtraTokenFields,
        openidconnect::core::CoreTokenType,
    >,
    openidconnect::core::CoreRevocableToken,
    openidconnect::StandardErrorResponse<openidconnect::RevocationErrorResponseType>,
>;

/// Inner type for [`Reminders`]
type ReminderInner = Arc<Mutex<VecDeque<(DateTime<Utc>, ReminderInstance)>>>;

/// The set of reminders that need to be sent out.
#[derive(Debug, Clone, Default)]
pub struct Reminders {
    inner: ReminderInner,
}

#[derive(Debug, Clone, Deserialize)]
struct HiBobOutResponse {
    outs: Vec<HiBobOutResponseField>,
}

#[derive(Debug, Clone, Deserialize)]
struct HiBobOutResponseField {
    #[serde(rename = "employeeEmail")]
    employee_email: String,
    #[serde(rename = "startDate")]
    start_date: NaiveDate,
    #[serde(rename = "endDate")]
    end_date: NaiveDate,
    #[serde(rename = "startDatePortion")]
    start_date_portion: String,
    #[serde(rename = "endDatePortion")]
    end_date_portion: String,
}

#[derive(Debug, Clone, Deserialize)]
struct HiBobPeopleResponse {
    employees: Vec<HiBobPeopleResponseField>,
}

#[derive(Debug, Clone, Deserialize)]
struct HiBobPeopleResponseField {
    email: String,
    personal: HiBobPeoplePersonalResponseField,
}

#[derive(Debug, Clone, Deserialize)]
struct HiBobPeoplePersonalResponseField {
    communication: HiBobPeoplePersonalCommunicationResponseField,
}

#[derive(Debug, Clone, Deserialize)]
struct HiBobPeoplePersonalCommunicationResponseField {
    #[serde(rename = "skypeUsername")]
    skype_username: Option<String>,
}

impl Reminders {
    /// Get how long until the next reminder needs to be sent.
    fn get_time_to_next(&self) -> Option<Duration> {
        let inner = self.inner.lock().expect("poisoned");

        inner.front().map(|(t, _)| *t - Utc::now())
    }

    /// Pop all reminders that are ready to be sent now.
    fn pop_due_reminders(&self) -> Vec<ReminderInstance> {
        let mut reminders = self.inner.lock().expect("poisoned");

        let mut due_reminders = Vec::new();
        let now = Utc::now();

        while let Some((date, reminder)) = reminders.pop_front() {
            info!(date = ?date, now = ?now, "Checking reminder");
            if date <= now {
                due_reminders.push(reminder);
            } else {
                reminders.push_front((date, reminder));
                break;
            }
        }

        due_reminders
    }

    /// Replace the current set of reminders
    fn replace(&self, reminders: VecDeque<(DateTime<Utc>, ReminderInstance)>) {
        let mut inner = self.inner.lock().expect("poisoned");

        *inner = reminders;
    }
}

#[derive(Debug, Deserialize)]
struct MatrixJoinResponse {
    room_id: String,
}

/// The high level app.
#[derive(Debug, Clone)]
pub struct App {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub database: Database,
    pub notify_db_update: Arc<Notify>,
    pub reminders: Reminders,
    pub email_to_matrix_id: Arc<Mutex<BTreeMap<String, String>>>,
    pub templates: Tera,
    sso_client: Option<OpenIDClient>,
}

impl App {
    pub async fn new(config: Config, database: Database, templates: Tera) -> Result<Self, Error> {
        let notify_db_update = Default::default();
        let reminders = Default::default();
        let email_to_matrix_id = Default::default();
        let http_client = Default::default();

        // Set up SSO
        let sso_client = if let Some(sso_config) = &config.sso {
            let provider_metadata = CoreProviderMetadata::discover_async(
                IssuerUrl::new(sso_config.issuer_url.clone())?,
                async_http_client,
            )
            .await?;

            let client = CoreClient::from_provider_metadata(
                provider_metadata,
                ClientId::new(sso_config.client_id.clone()),
                sso_config.client_secret.clone().map(ClientSecret::new),
            )
            // Set the URL the user will be redirected to after the authorization process.
            .set_redirect_uri(RedirectUrl::new(format!(
                "{}/sso_callback",
                &sso_config.base_url
            ))?);

            Some(client)
        } else {
            None
        };

        Ok(Self {
            config,
            http_client,
            database,
            notify_db_update,
            reminders,
            email_to_matrix_id,
            templates,
            sso_client,
        })
    }

    /// Start the background jobs, including sending reminders and updating calendars.
    pub async fn run(self) {
        tokio::join!(
            self.update_calendar_loop(),
            self.reminder_loop(),
            self.update_mappings_loop(),
            self.hibob_loop(),
        );
    }

    /// Fetches and stores updates for the stored calendars.
    #[instrument(skip(self))]
    pub async fn update_calendars(&self) -> Result<(), Error> {
        let db_calendars = self.database.get_calendars().await?;

        for db_calendar in db_calendars {
            let calendar_id = db_calendar.calendar_id;
            if let Err(error) = self.update_calendar(db_calendar).await {
                error!(
                    error = error.deref() as &dyn StdError,
                    calendar_id, "Failed to update calendar"
                );
            }
        }

        Ok(())
    }

    /// Update the given calendar we fetched from the DB.
    #[instrument(skip(self))]
    pub async fn update_calendar(&self, db_calendar: Calendar) -> Result<(), Error> {
        let calendars = fetch_calendars(
            &self.http_client,
            &db_calendar.url,
            db_calendar.user_name.as_deref(),
            db_calendar.password.as_deref(),
        )
        .await?;

        let mut vevents_by_id = HashMap::new();
        for calendar in &calendars {
            vevents_by_id.extend(&calendar.events);
        }

        let (events, next_dates) = parse_calendars_to_events(db_calendar.calendar_id, &calendars)?;

        // Some calendar systems (read: FastMail) create new events when people
        // edit the times for future events. Since we want the reminders to
        // apply to the new event we add some heuristics to detect this case and
        // copy across the reminders.
        let previous_events = self
            .database
            .get_events_in_calendar(db_calendar.calendar_id)
            .await?;

        let mut previous_events_by_id = HashMap::new();
        for (previous_event, _) in &previous_events {
            previous_events_by_id.insert(&previous_event.event_id, previous_event);
        }

        let mut events_by_summmary: HashMap<_, Vec<_>> = HashMap::new();
        let mut events_by_id = HashMap::new();
        for event in &events {
            events_by_summmary
                .entry((&event.summary, &event.organizer))
                .or_default()
                .push(event);
            events_by_id.insert(&event.event_id, event);
        }

        for (previous_event, _) in &previous_events {
            // Figure out if we should attempt to deduplicated based on this
            // event. We're either expecting it to not appear in the calendar or
            // for it to be a recurring event that has an end date.
            if let Some(existing_event) = vevents_by_id.get(&previous_event.event_id) {
                if let Some(recur) = &existing_event.base_event.recur {
                    match recur.end_condition {
                        EndCondition::Count(_) | EndCondition::Infinite => {
                            // The previous event hasn't been stopped, so we don't deduplicate.
                            continue;
                        }
                        EndCondition::Until(_) | EndCondition::UntilUtc(_) => {
                            // The previous event has been stopped, so we deduplicate.
                        }
                    }
                } else {
                    // Not a recurring event, so don't need to deduplicate.
                    continue;
                }
            }

            for new_event in events_by_summmary
                .get(&(&previous_event.summary, &previous_event.organizer))
                .map(|v| v.deref())
                .unwrap_or_else(|| &[])
            {
                if previous_event.event_id == new_event.event_id {
                    // This is just an event that we already have.
                    continue;
                }

                if previous_events_by_id.contains_key(&new_event.event_id) {
                    // We've already processed the new event.
                    continue;
                }

                let mut reminders = self
                    .database
                    .get_reminders_for_event(db_calendar.calendar_id, &previous_event.event_id)
                    .await?;

                // We only want to apply this logic for reminders that this user owns.
                reminders = reminders
                    .into_iter()
                    .filter(|r| r.user_id == db_calendar.user_id)
                    .collect();

                info!(
                    calendar_id = db_calendar.calendar_id,
                    prev_event = previous_event.event_id.deref(),
                    new_event = new_event.event_id.deref(),
                    reminders = reminders.len(),
                    "Found event duplicate, porting reminders."
                );

                for mut reminder in reminders {
                    reminder.reminder_id = -1;
                    reminder.event_id = new_event.event_id.clone();

                    self.database.add_reminder(reminder).await?;
                }
            }
        }

        self.database
            .insert_events(db_calendar.calendar_id, events, next_dates)
            .await?;

        self.update_reminders().await?;

        Ok(())
    }

    /// Queries the DB and updates the reminders
    #[instrument(skip(self))]
    pub async fn update_reminders(&self) -> Result<(), Error> {
        let reminders = self.database.get_next_reminders().await?;

        info!(num = reminders.len(), "Updated reminders");

        self.reminders.replace(reminders);
        self.notify_db_update.notify_one();

        Ok(())
    }

    /// Update the email to matrix ID mapping cache.
    #[instrument(skip(self))]
    async fn update_mappings(&self) -> Result<(), Error> {
        let mapping = self.database.get_user_mappings().await?;

        *self.email_to_matrix_id.lock().expect("poisoned") = mapping;

        Ok(())
    }

    /// An infinite loop that periodically triggers fetching updates for all
    /// calendars.
    async fn update_calendar_loop(&self) {
        let mut interval = interval(Duration::minutes(5).to_std().expect("std duration"));

        loop {
            interval.tick().await;

            if let Err(error) = self.update_calendars().await {
                error!(
                    error = error.deref() as &dyn StdError,
                    "Failed to update calendars"
                );
            }
        }
    }

    /// An infinite loop that periodically pulls changes to email to Matrix ID
    /// mappings from the DB.
    async fn update_mappings_loop(&self) {
        let mut interval = interval(Duration::minutes(5).to_std().expect("std duration"));

        loop {
            interval.tick().await;

            if let Err(error) = self.update_mappings().await {
                error!(
                    error = error.deref() as &dyn StdError,
                    "Failed to update mappings"
                );
            }
        }
    }

    /// Loop that handle sending the reminders.
    async fn reminder_loop(&self) {
        loop {
            let next_wakeup = self
                .reminders
                .get_time_to_next()
                .unwrap_or_else(|| Duration::minutes(5))
                .min(Duration::minutes(5));

            info!(
                time_to_next = ?next_wakeup,
                "Next reminder"
            );

            // `to_std` will fail if the duration is negative, but if that is
            // the case then we have due reminders that we can process
            // immediately.
            if let Ok(dur) = next_wakeup.to_std() {
                info!(
                    next_wakeup = ?next_wakeup,
                    "Sleeping for"
                );

                tokio::pin! {
                    let sleep_fut = sleep(dur);
                    let notify = self.notify_db_update.notified();
                }

                future::select(sleep_fut, notify).await;
            }

            let reminders = self.reminders.pop_due_reminders();

            info!(count = reminders.len(), "Due reminders");

            for reminder in reminders {
                info!(event_id = reminder.event_id.deref(), "Sending reminder");
                if let Err(err) = self.send_reminder(reminder).await {
                    error!(
                        error = err.deref() as &dyn StdError,
                        "Failed to send reminder"
                    );
                }
            }
        }
    }

    /// Send the reminder to the appropriate room.
    #[instrument(skip(self), fields(status))]
    async fn send_reminder(&self, reminder: ReminderInstance) -> Result<(), Error> {
        let join_url = format!(
            "{}/_matrix/client/r0/join/{}",
            self.config.matrix.homeserver_url,
            encode(&reminder.room),
        );

        let resp = self
            .http_client
            .post(&join_url)
            .bearer_auth(&self.config.matrix.access_token)
            .json(&json!({}))
            .send()
            .await
            .with_context(|| "Sending HTTP /join request")?;

        if !resp.status().is_success() {
            bail!("Got non-2xx from /join response: {}", resp.status());
        }

        let body: MatrixJoinResponse = resp.json().await?;

        let markdown_template = reminder.template.as_deref().unwrap_or(DEFAULT_TEMPLATE);

        let out_today_emails = self.database.get_out_today_emails().await?;

        let attendees = reminder
            .attendees
            .iter()
            .filter(|attendee| !out_today_emails.contains(&attendee.email))
            .map(|attendee| {
                if let Some(matrix_id) = self
                    .email_to_matrix_id
                    .lock()
                    .expect("poisoned")
                    .get(&attendee.email)
                {
                    format!(
                        "[{}](https://matrix.to/#/{})",
                        attendee.common_name.as_ref().unwrap_or(matrix_id),
                        matrix_id,
                    )
                } else {
                    attendee
                        .common_name
                        .as_ref()
                        .unwrap_or(&attendee.email)
                        .to_string()
                }
            })
            .join(", ");

        let handlebars = Handlebars::new();
        let markdown = handlebars
            .render_template(
                markdown_template,
                &json!({
                    "event_id": &reminder.event_id,
                    "summary": &reminder.summary,
                    "description": &reminder.description,
                    "location": &reminder.location,
                    "minutes_before": &reminder.minutes_before,
                    "attendees": attendees,
                }),
            )
            .with_context(|| "Rendering body template")?;

        let event_json = json!({
            "msgtype": "m.text",
            "body": markdown,
            "format": "org.matrix.custom.html",
            "formatted_body": markdown_to_html(&markdown, &ComrakOptions::default()),
        });

        let url = format!(
            "{}/_matrix/client/r0/rooms/{}/send/m.room.message",
            self.config.matrix.homeserver_url, body.room_id
        );

        let resp = self
            .http_client
            .post(&url)
            .bearer_auth(&self.config.matrix.access_token)
            .json(&event_json)
            .send()
            .await
            .with_context(|| "Sending HTTP send message request")?;

        Span::current().record("status", &resp.status().as_u16());

        info!(
            status = resp.status().as_u16(),
            event_id = reminder.event_id.deref(),
            room_id = body.room_id.deref(),
            "Sent reminder"
        );

        if !resp.status().is_success() {
            bail!("Got non-2xx from /send response: {}", resp.status());
        }

        Ok(())
    }

    /// An infinite loop that periodically pulls email to Matrix ID mappings and
    /// holidays from HiBob.
    async fn hibob_loop(&self) {
        let config = if let Some(config) = &self.config.hibob {
            config
        } else {
            return;
        };

        let mut interval = interval(Duration::minutes(5).to_std().expect("std duration"));

        loop {
            interval.tick().await;

            if let Err(error) = self.update_holidays(config).await {
                error!(
                    error = error.deref() as &dyn StdError,
                    "Failed to update holidays"
                );
            }

            if let Err(error) = self.update_email_mappings(config).await {
                error!(
                    error = error.deref() as &dyn StdError,
                    "Failed to update email mappings"
                );
            }
        }
    }

    /// Fetch who is on holiday today.
    #[instrument(skip(self, config), fields(status))]
    async fn update_holidays(&self, config: &HiBobConfig) -> Result<(), Error> {
        let resp = self
            .http_client
            .get("https://api.hibob.com/v1/timeoff/outtoday")
            .header("Authorization", &config.token)
            .header("Accepts", "application/json")
            .send()
            .await
            .with_context(|| "Sending HTTP /join request")?;

        Span::current().record("status", &resp.status().as_u16());

        info!(status = resp.status().as_u16(), "Got holidays response");

        if !resp.status().is_success() {
            bail!(
                "Got non-2xx from /timeoff/outtoday response: {}",
                resp.status()
            );
        }

        let parsed_response: HiBobOutResponse = resp.json().await?;

        let mut people_out = Vec::new();
        let today = Utc::today().naive_utc();

        for field in parsed_response.outs {
            if (field.start_date == today && field.start_date_portion != "all_day")
                || (field.end_date == today && field.end_date_portion != "all_day")
            {
                continue;
            }

            if field.start_date <= today && today <= field.end_date {
                people_out.push(field.employee_email);
            }
        }

        self.database.set_out_today(&people_out).await?;

        Ok(())
    }

    /// Fetch the email to Matrix ID mappings from HiBob.
    #[instrument(skip(self, config), fields(status))]
    async fn update_email_mappings(&self, config: &HiBobConfig) -> Result<(), Error> {
        let resp = self
            .http_client
            .get("https://api.hibob.com/v1/people")
            .header("Authorization", &config.token)
            .header("Accepts", "application/json")
            .send()
            .await
            .with_context(|| "Sending HTTP /join request")?;

        Span::current().record("status", &resp.status().as_u16());

        info!(status = resp.status().as_u16(), "Got people response");

        if !resp.status().is_success() {
            bail!("Got non-2xx from /people response: {}", resp.status());
        }

        let parsed_response: HiBobPeopleResponse = resp.json().await?;

        for employee in &parsed_response.employees {
            if let Some(matrix_id) = employee.personal.communication.skype_username.as_deref() {
                if is_likely_a_valid_user_id(matrix_id) {
                    let email = employee.email.as_str();
                    let new = self.database.add_matrix_id(email, matrix_id).await?;

                    if new {
                        info!(email, matrix_id, "Added new mapping");
                    }
                }
            }
        }

        Ok(())
    }

    /// Begin a new login with SSO session, returning the URL to redirect clients to.
    pub async fn start_login_via_sso(&self) -> Result<Url, Error> {
        let sso_client = self.sso_client.as_ref().context("SSO not configured")?;
        let sso_config = self.config.sso.as_ref().context("SSO not configured")?;

        // Generate a PKCE challenge.
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let mut request = sso_client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            // Set the PKCE code challenge.
            .set_pkce_challenge(pkce_challenge);

        // Set the desired scopes.
        for scope in &sso_config.scopes {
            request = request.add_scope(Scope::new(scope.to_string()));
        }

        // Generate the full authorization URL.
        let (auth_url, csrf_token, nonce) = request.url();

        self.database
            .add_sso_session(csrf_token.secret(), nonce.secret(), pkce_verifier.secret())
            .await?;

        Ok(auth_url)
    }

    /// Finish logging in via SSO, returning the email.
    pub async fn finish_login_via_sso(
        &self,
        state: String,
        auth_code: String,
    ) -> Result<String, Error> {
        let sso_client = self.sso_client.as_ref().context("SSO not configured")?;

        let (nonce_str, code_verifier) = self
            .database
            .claim_sso_session(&state)
            .await?
            .context("Unknown SSO session")?;
        let nonce = Nonce::new(nonce_str);
        let pkce_verifier = PkceCodeVerifier::new(code_verifier);

        let token_response = sso_client
            .exchange_code(AuthorizationCode::new(auth_code))
            // Set the PKCE code verifier.
            .set_pkce_verifier(pkce_verifier)
            .request_async(async_http_client)
            .await?;

        // Extract the ID token claims after verifying its authenticity and nonce.
        let id_token = token_response
            .id_token()
            .context("Server did not return an ID token")?;
        let claims = id_token.claims(&sso_client.id_token_verifier(), &nonce)?;

        // Verify the access token hash to ensure that the access token hasn't been substituted for
        // another user's.
        if let Some(expected_access_token_hash) = claims.access_token_hash() {
            let actual_access_token_hash = AccessTokenHash::from_token(
                token_response.access_token(),
                &id_token.signing_alg()?,
            )?;
            if actual_access_token_hash != *expected_access_token_hash {
                bail!("Invalid access token");
            }
        }

        let email = claims
            .email()
            .map(|email| email.as_str())
            .context("SSO didn't return an email")?;

        Ok(email.to_string())
    }

    /// Generate and persist a new access token for the user.
    pub async fn add_access_token(&self, user_id: i64) -> Result<String, Error> {
        let token: String = rand::thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();

        self.database
            .add_access_token(user_id, &token, Utc::now() + Duration::days(7))
            .await?;

        Ok(token)
    }
}

/// Checks if the string is likely a valid user ID.
///
/// Doesn't bother to fully check the domain part is valid
fn is_likely_a_valid_user_id(user_id: &str) -> bool {
    if user_id.len() < 2 {
        return false;
    }

    let sigil = &user_id[0..1];

    if sigil != "@" {
        return false;
    }

    let (local_part, domain) = if let Some(t) = user_id[1..].split_once(':') {
        t
    } else {
        return false;
    };

    // Assert that the localpart is printable ascii characters only (we don't
    // need to check it doesn't contain a colon, due to the above split). This
    // matches "historical" user IDs.
    if !local_part.bytes().all(|c| (0x21..=0x7E).contains(&c)) {
        return false;
    }

    // We don't bother doing a proper check of the domain part, as that is a bit
    // of a faff, so instead we do some rough checks like it doesn't contain
    // whitespace, etc.
    if !domain.chars().all(|c| {
        !c.is_whitespace()
            && !c.is_ascii_uppercase()
            && (c.is_ascii_alphanumeric() || "[]:.".contains(c))
    }) {
        return false;
    }

    true
}
