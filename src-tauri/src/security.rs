use serde::Serialize;
use url::Url;

pub(crate) const SLACK_APP_ORIGIN: &str = "https://app.slack.com";
pub(crate) const SLACK_START: &str = "https://app.slack.com/client";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destination {
    Workspace,
    SignIn,
    External,
    Blocked,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub(crate) enum OpenExternalUrlError {
    InvalidUrl,
    OpenFailed(String),
}

pub(crate) fn is_signin_page(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_https(url: &Url) -> bool {
    is_signin_page(url) && url.port().is_none()
}

pub(crate) fn is_app_origin(url: &Url) -> bool {
    is_https(url) && url.origin().ascii_serialization() == SLACK_APP_ORIGIN
}

pub(crate) fn is_client_page(url: &Url) -> bool {
    is_app_origin(url) && matches!(url.path().split('/').nth(1), Some("client" | "archives"))
}

fn is_slack_host(host: &str) -> bool {
    host == "slack.com" || host.ends_with(".slack.com")
}

fn route_matches(segment: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| {
        segment
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(['.', '_', '-']))
    })
}

fn account_route(path: &str) -> bool {
    let mut parts = path.split('/').skip(1);
    let first = parts.next().unwrap_or_default();
    route_matches(
        first,
        &[
            "signin",
            "signout",
            "sign_in",
            "sign_out",
            "workspace-signin",
            "get-started",
            "getstarted",
            "create",
            "ssb",
            "checkcookie",
            "checkmail",
            "confirm",
            "join",
            "shared_invite",
            "invite",
            "sso",
            "oauth",
            "openid",
            "forgot",
            "reset",
            "auth",
        ],
    ) || (first == "account"
        && route_matches(
            parts.next().unwrap_or_default(),
            &["signin", "signout", "sso"],
        ))
        || first.strip_prefix("x-").is_some_and(|rest| {
            let code = rest.split(['.', '_', '-']).next().unwrap_or_default();
            !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit())
        })
}

pub(crate) fn workspace_destination(url: &Url) -> Destination {
    if is_client_page(url) {
        return Destination::Workspace;
    }
    if is_https(url) {
        let host = url.host_str().unwrap_or_default();
        let workspace_host = host.ends_with(".slack.com")
            && !matches!(host, "www.slack.com" | "api.slack.com" | "files.slack.com");
        let path = url.path().to_ascii_lowercase();
        if is_slack_host(host)
            && (host == "join.slack.com"
                || account_route(&path)
                || (workspace_host
                    && (path == "/"
                        || matches!(path.split('/').nth(1), Some("client" | "archives")))))
        {
            return Destination::SignIn;
        }
    }
    if parse_external_url(url.as_str()).is_some() {
        Destination::External
    } else {
        Destination::Blocked
    }
}

pub(crate) fn is_signout(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    let mut parts = path.split('/').skip(1);
    let first = parts.next().unwrap_or_default();
    route_matches(first, &["signout", "sign_out"])
        || (first == "account" && route_matches(parts.next().unwrap_or_default(), &["signout"]))
}

pub(crate) fn is_signin_return(url: &Url) -> bool {
    is_app_origin(url) && url.path().split('/').nth(1) == Some("client")
}

pub(crate) fn valid_team(team: &str) -> bool {
    valid_slack_id(team, b"TE", 8)
}

pub(crate) fn valid_channel(channel: &str) -> bool {
    valid_slack_id(channel, b"CDG", 2)
}

fn valid_slack_id(id: &str, prefixes: &[u8], min_len: usize) -> bool {
    let bytes = id.as_bytes();
    (min_len..=32).contains(&bytes.len())
        && prefixes.contains(&bytes[0])
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

pub(crate) fn workspace_target(team: &str, url: Option<&str>) -> Option<Url> {
    if !valid_team(team) {
        return None;
    }
    let target = match url {
        Some(url) => Url::parse(url).ok()?,
        None => Url::parse(&format!("{SLACK_START}/{team}")).ok()?,
    };
    let mut path = target.path().split('/').skip(1);
    (is_app_origin(&target) && path.next() == Some("client") && path.next() == Some(team))
        .then_some(target)
}

pub(crate) fn parse_external_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    let allowed_scheme = matches!(url.scheme(), "http" | "https");
    let credentials_absent = url.username().is_empty() && url.password().is_none();

    (allowed_scheme && credentials_absent && url.host_str().is_some()).then_some(url)
}

#[tauri::command]
pub(crate) fn open_external_url(url: String) -> Result<(), OpenExternalUrlError> {
    let url = parse_external_url(&url).ok_or(OpenExternalUrlError::InvalidUrl)?;
    open::that(url.as_str()).map_err(|error| OpenExternalUrlError::OpenFailed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_client_sign_in_and_external_routes() {
        for (url, destination) in [
            (
                "https://app.slack.com/client/T0123456",
                Destination::Workspace,
            ),
            (
                "https://app.slack.com/archives/C0123456",
                Destination::Workspace,
            ),
            (
                "https://app.slack.com/workspace-signin",
                Destination::SignIn,
            ),
            ("https://team.slack.com/sso/saml", Destination::SignIn),
            (
                "https://team.slack.com/archives/C0123456",
                Destination::SignIn,
            ),
            ("https://team.slack.com/signout", Destination::SignIn),
            (
                "https://join.slack.com/t/example/shared_invite/example",
                Destination::SignIn,
            ),
            ("https://example.test/signin", Destination::External),
            ("https://app.slack.com:8443/client", Destination::External),
            (
                "https://app.slack.com.example.test/client",
                Destination::External,
            ),
            ("https://example@app.slack.com/client", Destination::Blocked),
            ("file:///tmp/example", Destination::Blocked),
        ] {
            assert_eq!(
                workspace_destination(&Url::parse(url).unwrap()),
                destination,
                "{url}"
            );
        }
    }

    #[test]
    fn workspace_destination_must_match_the_requested_team() {
        let team = "T0123456";
        assert!(workspace_target(team, None).is_some());
        assert!(workspace_target("E0123456", None).is_some());
        assert!(workspace_target("team.slack.com", None).is_none());
        for url in [
            "https://app.slack.com/client/T0123456/C0123456?thread=1#message",
            "https://app.slack.com:443/client/T0123456",
        ] {
            assert!(workspace_target(team, Some(url)).is_some(), "{url}");
        }
        for url in [
            "https://app.slack.com/client/T0678901",
            "https://app.slack.com/client/T0123456-other",
            "https://team.slack.com/client/T0123456",
            "https://app.slack.com/signin",
            "https://app.slack.com:8443/client/T0123456",
        ] {
            assert!(workspace_target(team, Some(url)).is_none(), "{url}");
        }
    }

    #[test]
    fn sign_in_suffixes_and_signout_match_the_preload_routes() {
        let sign_in = Url::parse("https://slack.com/signin-find").unwrap();
        let signout = Url::parse("https://team.slack.com/account/signout-confirm").unwrap();
        assert_eq!(workspace_destination(&sign_in), Destination::SignIn);
        assert_eq!(workspace_destination(&signout), Destination::SignIn);
        assert!(is_signout(&signout));
    }

    #[test]
    fn sign_in_can_handoff_only_to_the_app_client() {
        for url in [
            "https://app.slack.com/client",
            "https://app.slack.com/client/T0123456?login=complete#home",
        ] {
            assert!(is_signin_return(&Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "https://app.slack.com/auth",
            "https://team.slack.com/client/T0123456",
            "https://example.test/client",
        ] {
            assert!(!is_signin_return(&Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn external_urls_allow_only_http_and_https_without_credentials() {
        assert!(parse_external_url("https://example.test/docs").is_some());
        assert!(parse_external_url("http://example.test/docs").is_some());
        assert!(parse_external_url("file:///tmp/example").is_none());
        assert!(parse_external_url("custom-protocol:example").is_none());
        assert!(parse_external_url("https://user@example.test/docs").is_none());
    }
}
