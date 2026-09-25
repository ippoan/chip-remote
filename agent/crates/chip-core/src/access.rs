//! Telling a Cloudflare Access rejection apart from the Worker's own answers.
//!
//! Access sits in front of the Worker (docs/PROTOCOL.md 「認証」). Without (or with wrong)
//! service-token headers it answers before the Worker sees the request: a redirect to
//! `<team>.cloudflareaccess.com` (login page) or a 401 / 403 HTML page. The Worker itself
//! never redirects and its 401 is JSON (`{"error":"unauthorized"}`).

/// Logged (and shown) when Access rejects the agent. Contains neither id nor secret.
pub const ACCESS_DENIED_MESSAGE: &str =
    "Cloudflare Access に拒否されました (accessClientId / accessClientSecret を確認)";

/// Whether `location` (a `Location` header value) points at `*.cloudflareaccess.com`.
pub fn is_access_location(location: &str) -> bool {
    let l = location.trim();
    let Some((_, rest)) = l.split_once("://") else {
        return false; // relative redirect: same host, not Access
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = host
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    host == "cloudflareaccess.com" || host.ends_with(".cloudflareaccess.com")
}

/// Whether an HTTP answer (to the WS upgrade or an API call) came from Access rather
/// than the Worker: a redirect to `*.cloudflareaccess.com`, or 401 / 403 that is not
/// the Worker's JSON error.
pub fn is_access_rejection(
    status: u16,
    location: Option<&str>,
    content_type: Option<&str>,
) -> bool {
    match status {
        300..=399 => location.is_some_and(is_access_location),
        401 | 403 => !content_type.is_some_and(|c| c.to_ascii_lowercase().contains("json")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_locations() {
        assert!(is_access_location(
            "https://ippoan.cloudflareaccess.com/cdn-cgi/access/login/chip-remote.ippoan.org?kid=x"
        ));
        assert!(is_access_location("https://IPPOAN.CloudflareAccess.com"));
        assert!(is_access_location(
            "https://team.cloudflareaccess.com:443/x"
        ));
        assert!(!is_access_location("/v1/agent/ws"));
        assert!(!is_access_location("https://chip-remote.ippoan.org/x"));
        assert!(!is_access_location("https://evilcloudflareaccess.com/"));
        assert!(!is_access_location(
            "https://chip-remote.ippoan.org/?next=https://a.cloudflareaccess.com"
        ));
        assert!(!is_access_location(
            "https://cloudflareaccess.com.evil.example/"
        ));
    }

    #[test]
    fn rejections() {
        let loc = Some("https://ippoan.cloudflareaccess.com/cdn-cgi/access/login/x");
        assert!(is_access_rejection(302, loc, None));
        assert!(is_access_rejection(307, loc, Some("text/html")));
        assert!(!is_access_rejection(
            302,
            Some("https://elsewhere.example/"),
            None
        ));
        assert!(!is_access_rejection(302, None, None));
        assert!(is_access_rejection(
            403,
            None,
            Some("text/html; charset=UTF-8")
        ));
        assert!(is_access_rejection(401, None, None));
        // The Worker's own 401.
        assert!(!is_access_rejection(401, None, Some("application/json")));
        assert!(!is_access_rejection(
            401,
            None,
            Some("Application/JSON; charset=UTF-8")
        ));
        assert!(!is_access_rejection(200, loc, None));
        assert!(!is_access_rejection(503, None, Some("text/html")));
    }
}
