//! `inbox` commands: email inboxes and the emails they capture, so CI and
//! agents can read a signup code or magic link without curl or jq.

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use clap::{Args, Subcommand};
use std::time::{Duration, Instant};

use crate::api::{self, ApiClient, EmailFilters};
use crate::cli::{format_duration, parse_duration_within};
use crate::commands::endpoint::resolve_since;
use crate::config;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::errors::{EmailValue, EmailValueMissing, EmailWaitTimeout};
use crate::output::Stylize;
use crate::render::{
    OutputField, OutputStatus, TerminalTextLayout, new_table, output_field, print_body_section,
    print_context, print_empty_state, print_field, print_json, print_pagination, print_status,
    print_status_block, sanitize_terminal, value_or_dash,
};

const WAIT_AFTER_HELP: &str = "\
Without --since, only emails that arrive after the command starts match:
start it before triggering the email, or pass --since.

EXIT STATUS:
  0  An email matched and was printed
  5  --timeout passed before a matching email arrived
  6  The email that matched has no code (--code) or link (--link)
  1  Any other error (2 for invalid arguments)

EXAMPLE:
  ADDR=\"signup+$RUN_ID@hookinbox.com\"
  STARTED=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  ./sign-up.sh \"$ADDR\"
  CODE=$(hooklistener inbox wait \"$ADDR\" --since \"$STARTED\" --code)";

#[derive(Subcommand)]
pub enum InboxAction {
    /// Create an email inbox and print its address
    Create {
        /// Inbox display name
        name: String,
        /// Custom slug: the part of the address before the @
        #[arg(long)]
        slug: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List email inboxes for an organization
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List the emails an inbox received, newest first
    #[command(name = "list-emails", visible_alias = "emails")]
    ListEmails {
        /// Inbox ID, slug, or address; a plus address such as signup+run42@hookinbox.com also sets --to
        inbox: String,
        #[command(flatten)]
        filters: EmailFilterArgs,
        /// Only emails received after this: an RFC 3339 time, an email ID, or a duration ago such as 30m
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page, up to 100
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
        page_size: u32,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show an email with its code, link, and text
    #[command(name = "show-email", visible_alias = "email")]
    ShowEmail {
        /// Inbox ID, slug, or address
        inbox: String,
        /// Email ID (from `inbox emails` or `inbox wait`)
        email_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Wait for an email to reach an inbox, then print it, its code, or its link
    ///
    /// Made for CI and agents. Prints a short summary of the first matching
    /// email, or with --code or --link only that value, so a script can
    /// capture it with $(...). Calls longer than the server's one-minute limit
    /// repeat with the same --since, so an email that lands between two calls
    /// still matches.
    #[command(after_help = WAIT_AFTER_HELP)]
    Wait {
        /// Inbox ID, slug, or address; a plus address such as signup+run42@hookinbox.com also sets --to
        inbox: String,
        #[command(flatten)]
        filters: EmailFilterArgs,
        /// Match emails received after this: an RFC 3339 time, an email ID, or a duration ago such as 90s or 5m [default: now]
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
        /// Maximum wait, such as 90, 5m, or 1h (up to 1h) [default: 2m]
        #[arg(long, value_name = "DURATION", value_parser = parse_duration_within(Duration::ZERO, MAX_EMAIL_WAIT))]
        timeout: Option<Duration>,
        /// Print only the one-time code
        #[arg(long, conflicts_with = "link")]
        code: bool,
        /// Print only the verification or magic link
        #[arg(long)]
        link: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

/// Filters shared by `inbox list-emails` and `inbox wait`.
#[derive(Args)]
pub struct EmailFilterArgs {
    /// Recipient address, exact and case-insensitive, such as a per-run plus address
    #[arg(long, value_name = "ADDRESS")]
    pub to: Option<String>,
    /// Text the sender contains, case-insensitive
    #[arg(long, value_name = "TEXT")]
    pub from: Option<String>,
    /// Text the subject contains, case-insensitive
    #[arg(long, value_name = "TEXT")]
    pub subject_contains: Option<String>,
}

impl EmailFilterArgs {
    /// The API filters. `recipient`, from a plus address given as the inbox,
    /// applies when --to is not given.
    pub fn into_filters(self, since: Option<String>, recipient: Option<String>) -> EmailFilters {
        EmailFilters {
            since,
            to: self.to.or(recipient),
            from: self.from,
            subject_contains: self.subject_contains,
        }
    }
}

const DEFAULT_EMAIL_WAIT: Duration = Duration::from_secs(120);
const MAX_EMAIL_WAIT: Duration = Duration::from_secs(60 * 60);
/// The server holds one wait call for at most a minute.
const MAX_SERVER_WAIT_SECONDS: u64 = 60;

pub async fn execute(action: InboxAction, json: bool) -> Result<()> {
    match action {
        InboxAction::Create { name, slug, org } => {
            let (client, organization_id) = connect(org).await?;
            let inbox = client.create_inbox(&name, slug.as_deref()).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "inbox": inbox
                }))?;
            } else {
                print_status(OutputStatus::Ok, "INBOX CREATED");
                println!();
                print_fields(&inbox_detail_fields(&inbox));
                print_field("ORGANIZATION", inline(&organization_id));
                print_field(
                    "ACTION",
                    format!(
                        "Run `hooklistener inbox wait {}` before triggering an email",
                        inline(&inbox.slug)
                    )
                    .dim(),
                );
            }
        }
        InboxAction::List { org } => {
            let (client, organization_id) = connect(org).await?;
            let inboxes = client.list_inboxes().await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "inboxes": inboxes
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_inboxes(&inboxes);
            }
        }
        InboxAction::ListEmails {
            inbox,
            filters,
            since,
            page,
            page_size,
            org,
        } => {
            let since = since
                .as_deref()
                .map(|since| resolve_since(Some(since), Utc::now()))
                .transpose()?;
            let (client, organization_id) = connect(org).await?;
            let target = resolve_inbox(&client, &inbox).await?;
            let filters = filters.into_filters(since, target.recipient);
            let emails = client
                .list_inbox_emails(&target.id, &filters, page, page_size)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "inbox_id": target.id,
                    "emails": emails
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Inbox:", &target.id);
                print_inbox_emails(&emails);
            }
        }
        InboxAction::ShowEmail {
            inbox,
            email_id,
            org,
        } => {
            let (client, organization_id) = connect(org).await?;
            let target = resolve_inbox(&client, &inbox).await?;
            let email = client.get_inbox_email(&target.id, &email_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "inbox_id": target.id,
                    "email": email
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Inbox:", &target.id);
                print_email_detail(&email);
            }
        }
        InboxAction::Wait {
            inbox,
            filters,
            since,
            timeout,
            code,
            link,
            org,
        } => {
            let value = wanted_value(code, link);
            if let Some(value) = value
                && json
            {
                bail!(
                    "inbox wait {flag} does not support --json: it prints only that value. Drop --json, or drop {flag} and read email.{field} from the JSON.",
                    flag = value.flag(),
                    field = value.field()
                );
            }
            // Noted before signing in and finding the inbox, so an email sent
            // meanwhile still matches.
            let since = resolve_since(since.as_deref(), Utc::now())?;
            let timeout = timeout.unwrap_or(DEFAULT_EMAIL_WAIT);
            let (client, organization_id) = connect(org).await?;
            let target = resolve_inbox(&client, &inbox).await?;
            let filters = filters.into_filters(Some(since), target.recipient);

            let email = wait_for_email(&client, &target.id, &filters, timeout)
                .await?
                .ok_or_else(|| EmailWaitTimeout {
                    inbox: inbox.clone(),
                    timeout: format_duration(timeout),
                })?;

            if json {
                print_json(&serde_json::json!({
                    "status": "received",
                    "organization_id": organization_id,
                    "inbox_id": target.id,
                    "email": email
                }))?;
            } else if let Some(value) = value {
                println!("{}", email_value(&email, value, &inbox)?);
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "EMAIL RECEIVED",
                    &email_summary_fields(&email),
                );
            }
        }
    }
    Ok(())
}

async fn connect(org: Option<String>) -> Result<(ApiClient, String)> {
    let mut config = config::Config::load()?;
    let organization_id = require_organization(org, &config)?;
    let token = ensure_valid_token(&mut config).await?;
    let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
    Ok((client, organization_id))
}

/// The value `--code` or `--link` asks `inbox wait` to print, if any.
pub fn wanted_value(code: bool, link: bool) -> Option<EmailValue> {
    match (code, link) {
        (true, _) => Some(EmailValue::Code),
        (_, true) => Some(EmailValue::Link),
        _ => None,
    }
}

/// An inbox named on the command line, resolved to its ID.
#[derive(Debug, PartialEq, Eq)]
pub struct InboxTarget {
    pub id: String,
    /// The address given when it is a plus address of the inbox, such as
    /// `signup+run42@hookinbox.com`: emails are then filtered to it.
    pub recipient: Option<String>,
}

/// Resolves an inbox ID, slug, or address. An ID is used as given; a slug or
/// address is looked up among the organization's inboxes.
pub async fn resolve_inbox(client: &ApiClient, inbox: &str) -> Result<InboxTarget> {
    let inbox = inbox.trim();
    if is_uuid(inbox) {
        return Ok(InboxTarget {
            id: inbox.to_string(),
            recipient: None,
        });
    }

    let inboxes = client.list_inboxes().await?;
    match_inbox(&inboxes, inbox).ok_or_else(|| {
        anyhow!(
            "No inbox matches '{inbox}'. Run `hooklistener inbox list` to see inbox IDs, slugs, and addresses."
        )
    })
}

/// Inbox IDs are hyphenated UUIDs; slugs are lowercase letters, digits, and
/// hyphens, so a slug could be mistaken for an ID only if it were one.
fn is_uuid(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::try_parse(value).is_ok()
}

/// Finds the inbox whose slug or address is `wanted`, ignoring case. A plus
/// address matches the inbox it delivers to and becomes the recipient filter.
pub fn match_inbox(inboxes: &[api::EmailInbox], wanted: &str) -> Option<InboxTarget> {
    let base = base_address(wanted);
    inboxes.iter().find_map(|inbox| {
        let recipient = if inbox.slug.eq_ignore_ascii_case(wanted)
            || inbox.email_address.eq_ignore_ascii_case(wanted)
        {
            None
        } else if base
            .as_deref()
            .is_some_and(|base| inbox.email_address.eq_ignore_ascii_case(base))
        {
            Some(wanted.to_string())
        } else {
            return None;
        };
        Some(InboxTarget {
            id: inbox.id.clone(),
            recipient,
        })
    })
}

/// `signup@hookinbox.com` for `signup+run42@hookinbox.com`; `None` for
/// anything that is not a plus address.
fn base_address(value: &str) -> Option<String> {
    let (local, domain) = value.rsplit_once('@')?;
    let (base, _tag) = local.split_once('+')?;
    Some(format!("{base}@{domain}"))
}

/// Long-polls until an email matches `filters` or `timeout` passes; `None`
/// on timeout. Each call waits at most a minute, and later calls keep the
/// same `since`, so an email that lands between two calls still matches.
pub async fn wait_for_email(
    client: &ApiClient,
    inbox_id: &str,
    filters: &EmailFilters,
    timeout: Duration,
) -> Result<Option<api::InboxEmail>> {
    let deadline = Instant::now() + timeout;
    loop {
        let seconds = deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .min(MAX_SERVER_WAIT_SECONDS);

        if let Some(email) = client
            .wait_for_inbox_email(inbox_id, filters, seconds)
            .await?
        {
            return Ok(Some(email));
        }

        if deadline.saturating_duration_since(Instant::now()) < Duration::from_secs(1) {
            return Ok(None);
        }
    }
}

/// The code or link `inbox wait` prints alone, made terminal-safe, or the
/// error that says the email has none.
pub fn email_value(
    email: &api::InboxEmail,
    value: EmailValue,
    inbox: &str,
) -> std::result::Result<String, EmailValueMissing> {
    let found = match value {
        EmailValue::Code => email.code.as_deref(),
        EmailValue::Link => email.verification_link.as_deref(),
    };
    found
        .map(str::trim)
        .filter(|found| !found.is_empty())
        .map(inline)
        .ok_or_else(|| EmailValueMissing::new(value, inbox, &email.id, email.subject.as_deref()))
}

fn inline(value: &str) -> String {
    sanitize_terminal(value, TerminalTextLayout::Inline).into_owned()
}

fn print_fields(fields: &[OutputField]) {
    for field in fields {
        print_field(field.label, &field.value);
    }
}

pub(crate) fn inbox_detail_fields(inbox: &api::EmailInbox) -> Vec<OutputField> {
    let mut fields = vec![
        output_field("ID", inline(&inbox.id)),
        output_field("NAME", inline(&inbox.name)),
        output_field("SLUG", inline(&inbox.slug)),
        output_field("ADDRESS", inline(&inbox.email_address).bold()),
    ];
    if let Some(count) = inbox.email_count {
        fields.push(output_field("EMAILS", count));
    }
    if let Some(created_at) = inbox.created_at.as_deref() {
        fields.push(output_field("CREATED AT", inline(created_at).dim()));
    }
    fields
}

/// The short summary `inbox wait` prints, and the head of `show-email`.
pub(crate) fn email_summary_fields(email: &api::InboxEmail) -> Vec<OutputField> {
    let recipients = if email.recipients.is_empty() {
        "-".to_string()
    } else {
        email.recipients.join(", ")
    };
    vec![
        output_field("EMAIL ID", inline(&email.id)),
        output_field("FROM", inline(value_or_dash(email.from.as_deref()))),
        output_field("TO", inline(&recipients)),
        output_field("SUBJECT", inline(value_or_dash(email.subject.as_deref()))),
        output_field(
            "RECEIVED AT",
            inline(value_or_dash(email.created_at.as_deref())).dim(),
        ),
        output_field(
            "CODE",
            match email.code.as_deref() {
                Some(code) => inline(code).bold().to_string(),
                None => "(none)".dim().to_string(),
            },
        ),
        output_field(
            "LINK",
            match email.verification_link.as_deref() {
                Some(link) => inline(link).underlined().to_string(),
                None => "(none)".dim().to_string(),
            },
        ),
    ]
}

/// `show-email`: the summary, every code when there are several, the
/// attachments, then the text body.
pub(crate) fn email_detail_fields(email: &api::InboxEmail) -> Vec<OutputField> {
    let mut fields = email_summary_fields(email);
    if email.codes.len() > 1 {
        fields.push(output_field("CODES", inline(&email.codes.join(", "))));
    }
    if !email.attachments.is_empty() {
        let names = email
            .attachments
            .iter()
            .map(|attachment| {
                attachment
                    .get("filename")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("(unnamed)")
            })
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(output_field("ATTACHMENTS", inline(&names)));
    }
    fields
}

fn print_email_detail(email: &api::InboxEmail) {
    print_fields(&email_detail_fields(email));
    println!();
    print_body_section("Text:", email.text.as_deref());
}

/// IDs are what other commands take, so they stay on one line to copy.
fn keep_ids_whole(table: &mut comfy_table::Table) {
    if let Some(column) = table.column_mut(0) {
        column.set_constraint(comfy_table::ColumnConstraint::ContentWidth);
    }
}

pub fn print_inboxes(inboxes: &[api::EmailInbox]) {
    if inboxes.is_empty() {
        print_empty_state(
            "NO INBOXES FOUND",
            "Run `hooklistener inbox create <name>` to create one.",
        );
        return;
    }

    // The address starts with the slug, so the slug gets no column of its own.
    let mut table = new_table(&["ID", "Address", "Emails", "Name"]);
    for inbox in inboxes {
        table.add_row(vec![
            inline(&inbox.id),
            inline(&inbox.email_address),
            inbox
                .email_count
                .map_or_else(|| "-".to_string(), |count| count.to_string()),
            inline(&inbox.name),
        ]);
    }
    keep_ids_whole(&mut table);
    println!("{table}");
}

pub fn print_inbox_emails(response: &api::InboxEmailsResponse) {
    if response.data.is_empty() {
        print_empty_state(
            "NO EMAILS FOUND",
            "Send an email to the inbox address, or run `hooklistener inbox wait <inbox>` to wait for one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Received", "From", "Subject"]);
    for email in &response.data {
        table.add_row(vec![
            inline(&email.id),
            inline(value_or_dash(email.created_at.as_deref())),
            inline(value_or_dash(email.from.as_deref())),
            inline(value_or_dash(email.subject.as_deref())),
        ]);
    }
    keep_ids_whole(&mut table);
    println!("{table}");
    print_pagination(&response.pagination);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{command_exit_code, error_code};

    const INBOX_ID: &str = "5f0c6e2a-1b2c-4d5e-8f90-123456789abc";
    const WAIT_PATH: &str = "/api/v1/inboxes/5f0c6e2a-1b2c-4d5e-8f90-123456789abc/emails/wait";
    const EMAIL_BODY: &str = r#"{"data":{"id":"em-1","subject":"Your code","code":"482913","verification_link":"https://acme.test/verify?t=1"}}"#;

    fn client(server: &mockito::Server) -> ApiClient {
        ApiClient::with_base_url("test-token".to_string(), server.url(), None).unwrap()
    }

    fn inbox(id: &str, slug: &str) -> api::EmailInbox {
        api::EmailInbox {
            id: id.to_string(),
            name: slug.to_string(),
            slug: slug.to_string(),
            email_address: format!("{slug}@hookinbox.com"),
            email_count: Some(0),
            created_at: None,
        }
    }

    fn email(code: Option<&str>, link: Option<&str>) -> api::InboxEmail {
        serde_json::from_value(serde_json::json!({
            "id": "em-1",
            "subject": "Welcome to Acme",
            "code": code,
            "verification_link": link,
        }))
        .unwrap()
    }

    fn since_filters() -> EmailFilters {
        EmailFilters {
            since: Some("2026-10-12T12:00:00.000Z".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn wait_repeats_after_no_content_with_the_same_since() {
        let mut server = mockito::Server::new_async().await;
        let same_since =
            mockito::Matcher::UrlEncoded("since".into(), "2026-10-12T12:00:00.000Z".into());
        let nothing_yet = server
            .mock("GET", WAIT_PATH)
            .match_query(same_since.clone())
            .with_status(204)
            .expect(1)
            .create_async()
            .await;
        let arrived = server
            .mock("GET", WAIT_PATH)
            .match_query(same_since)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EMAIL_BODY)
            .expect(1)
            .create_async()
            .await;

        let email = wait_for_email(
            &client(&server),
            INBOX_ID,
            &since_filters(),
            Duration::from_secs(10),
        )
        .await
        .unwrap()
        .expect("the second call returns the email");

        assert_eq!(email.id, "em-1");
        nothing_yet.assert_async().await;
        arrived.assert_async().await;
    }

    #[tokio::test]
    async fn wait_caps_each_call_at_the_server_limit() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", WAIT_PATH)
            .match_query(mockito::Matcher::UrlEncoded("timeout".into(), "60".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EMAIL_BODY)
            .create_async()
            .await;

        let email = wait_for_email(
            &client(&server),
            INBOX_ID,
            &since_filters(),
            Duration::from_secs(600),
        )
        .await
        .unwrap();

        assert!(email.is_some());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn wait_gives_up_after_the_timeout() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", WAIT_PATH)
            .match_query(mockito::Matcher::UrlEncoded("timeout".into(), "0".into()))
            .with_status(204)
            .expect(1)
            .create_async()
            .await;

        let email = wait_for_email(&client(&server), INBOX_ID, &since_filters(), Duration::ZERO)
            .await
            .unwrap();

        assert!(email.is_none(), "a zero timeout checks once and stops");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn wait_stops_on_server_errors() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", WAIT_PATH)
            .match_query(mockito::Matcher::Any)
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"since: no email with that id in this inbox"}"#)
            .expect(1)
            .create_async()
            .await;

        let err = wait_for_email(
            &client(&server),
            INBOX_ID,
            &since_filters(),
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("no email with that id"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn an_inbox_id_is_used_without_listing_inboxes() {
        let mut server = mockito::Server::new_async().await;
        let list = server
            .mock("GET", "/api/v1/inboxes")
            .expect(0)
            .create_async()
            .await;

        let target = resolve_inbox(&client(&server), &format!(" {INBOX_ID} "))
            .await
            .unwrap();

        assert_eq!(
            target,
            InboxTarget {
                id: INBOX_ID.to_string(),
                recipient: None
            }
        );
        list.assert_async().await;
    }

    #[tokio::test]
    async fn a_slug_or_address_is_resolved_through_the_inbox_list() {
        let mut server = mockito::Server::new_async().await;
        let list = server
            .mock("GET", "/api/v1/inboxes")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":[{"id":"in-1","name":"Signup","slug":"signup","email_address":"signup@hookinbox.com"}]}"#,
            )
            .expect(2)
            .create_async()
            .await;
        let client = client(&server);

        let target = resolve_inbox(&client, "signup+run42@hookinbox.com")
            .await
            .unwrap();
        assert_eq!(target.id, "in-1");
        assert_eq!(
            target.recipient.as_deref(),
            Some("signup+run42@hookinbox.com")
        );

        let err = resolve_inbox(&client, "billing").await.unwrap_err();
        assert!(err.to_string().contains("No inbox matches 'billing'"));
        assert!(err.to_string().contains("hooklistener inbox list"));
        list.assert_async().await;
    }

    #[test]
    fn inboxes_match_by_slug_address_or_plus_address() {
        let inboxes = [inbox("in-1", "signup"), inbox("in-2", "billing")];
        let target = |id: &str, recipient: Option<&str>| {
            Some(InboxTarget {
                id: id.to_string(),
                recipient: recipient.map(str::to_string),
            })
        };

        assert_eq!(match_inbox(&inboxes, "billing"), target("in-2", None));
        assert_eq!(match_inbox(&inboxes, "SIGNUP"), target("in-1", None));
        assert_eq!(
            match_inbox(&inboxes, "Signup@HookInbox.com"),
            target("in-1", None)
        );
        assert_eq!(
            match_inbox(&inboxes, "signup+run42@hookinbox.com"),
            target("in-1", Some("signup+run42@hookinbox.com"))
        );
        assert_eq!(match_inbox(&inboxes, "signup+run42@example.com"), None);
        assert_eq!(match_inbox(&inboxes, "sign"), None);
        assert_eq!(match_inbox(&inboxes, "in-1"), None, "IDs are not looked up");
    }

    #[test]
    fn an_explicit_to_wins_over_the_plus_address() {
        let args = |to: Option<&str>| EmailFilterArgs {
            to: to.map(str::to_string),
            from: Some("acme".into()),
            subject_contains: None,
        };
        let recipient = Some("signup+run42@hookinbox.com".to_string());

        assert_eq!(
            args(None).into_filters(None, recipient.clone()).to,
            recipient
        );
        let filters = args(Some("other@hookinbox.com")).into_filters(Some("5m".into()), recipient);
        assert_eq!(
            filters,
            EmailFilters {
                since: Some("5m".into()),
                to: Some("other@hookinbox.com".into()),
                from: Some("acme".into()),
                subject_contains: None,
            }
        );
    }

    #[test]
    fn code_and_link_print_alone() {
        let found = email(Some("482913"), Some("https://acme.test/verify?t=1"));

        assert_eq!(wanted_value(false, false), None);
        assert_eq!(wanted_value(true, false), Some(EmailValue::Code));
        assert_eq!(wanted_value(false, true), Some(EmailValue::Link));
        assert_eq!(
            email_value(&found, EmailValue::Code, "signup").unwrap(),
            "482913"
        );
        assert_eq!(
            email_value(&found, EmailValue::Link, "signup").unwrap(),
            "https://acme.test/verify?t=1"
        );

        let hostile = email(None, Some("https://acme.test/\u{1b}]52;c;x\u{7}"));
        let link = email_value(&hostile, EmailValue::Link, "signup").unwrap();
        assert!(!link.contains('\u{1b}'), "control characters are escaped");
    }

    #[test]
    fn a_missing_code_or_link_has_its_own_exit_status() {
        let without = email(None, Some("  "));

        for (value, code) in [
            (EmailValue::Code, "code_not_found"),
            (EmailValue::Link, "link_not_found"),
        ] {
            let err: anyhow::Error = email_value(&without, value, "signup").unwrap_err().into();
            assert_eq!(error_code(&err), code);
            assert_eq!(command_exit_code(&err), 6);
            assert!(err.to_string().contains("em-1"));
            assert!(err.to_string().contains("Welcome to Acme"));
            assert!(
                crate::errors::error_hint(&err)
                    .is_some_and(|hint| hint.contains("inbox show-email signup em-1"))
            );
        }
    }

    #[test]
    fn a_timeout_has_its_own_exit_status() {
        let err: anyhow::Error = EmailWaitTimeout {
            inbox: "signup".into(),
            timeout: "2m".into(),
        }
        .into();

        assert_eq!(error_code(&err), "wait_timeout");
        assert_eq!(command_exit_code(&err), 5);
        assert_eq!(
            err.to_string(),
            "No matching email reached inbox signup within 2m."
        );
        assert!(crate::errors::error_hint(&err).is_some_and(|hint| hint.contains("--since")));
    }
}
