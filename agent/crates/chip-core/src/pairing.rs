//! Connection code for the phone app (tray 「スマホ接続用 QR を表示」).
//!
//! `chipremote:` + base64url (no padding) of UTF-8 JSON
//! `{"url","token","accessClientId","accessClientSecret"}`. The access keys are left
//! out unless both are set (same rule as the headers, [`Config::access_headers`]).
//! The Android app parses the same format. The code contains the token: never log it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Serialize;

use crate::Config;

/// Scheme-like prefix of a connection code.
pub const CONNECT_CODE_PREFIX: &str = "chipremote:";

#[derive(Serialize)]
struct Payload<'a> {
    url: &'a str,
    token: &'a str,
    #[serde(rename = "accessClientId", skip_serializing_if = "Option::is_none")]
    access_client_id: Option<&'a str>,
    #[serde(rename = "accessClientSecret", skip_serializing_if = "Option::is_none")]
    access_client_secret: Option<&'a str>,
}

/// The connection code for `cfg`, or None when url / token are not set (empty or the
/// config.example.json placeholder). Values are trimmed, the url's trailing `/` dropped.
pub fn connect_code(cfg: &Config) -> Option<String> {
    let url = cfg.url.trim().trim_end_matches('/');
    let token = cfg.token.trim();
    if url.is_empty() || token.is_empty() || token.starts_with("REPLACE_") {
        return None;
    }
    let access = cfg.access_headers();
    let payload = Payload {
        url,
        token,
        access_client_id: access.map(|a| a[0].1),
        access_client_secret: access.map(|a| a[1].1),
    };
    let json = serde_json::to_string(&payload).ok()?;
    Some(format!(
        "{CONNECT_CODE_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn raw(code: &str) -> String {
        let b64 = code.strip_prefix(CONNECT_CODE_PREFIX).expect("prefix");
        assert!(
            b64.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "base64url without padding: {b64}"
        );
        String::from_utf8(URL_SAFE_NO_PAD.decode(b64).unwrap()).unwrap()
    }

    fn decode(code: &str) -> Value {
        serde_json::from_str(&raw(code)).unwrap()
    }

    fn cfg(url: &str, token: &str) -> Config {
        Config {
            url: url.into(),
            token: token.into(),
            ..Config::default()
        }
    }

    #[test]
    fn without_access_keys() {
        let code = connect_code(&cfg(" https://chip-remote.ippoan.org/ ", " tok\r\n")).unwrap();
        assert_eq!(
            decode(&code),
            json!({"url":"https://chip-remote.ippoan.org","token":"tok"})
        );
    }

    #[test]
    fn with_access_keys() {
        let c = Config {
            access_client_id: " id.access ".into(),
            access_client_secret: "sec".into(),
            ..cfg("https://a", "t")
        };
        let code = connect_code(&c).unwrap();
        // Key order as documented (the Android side does not depend on it).
        assert_eq!(
            raw(&code),
            r#"{"url":"https://a","token":"t","accessClientId":"id.access","accessClientSecret":"sec"}"#
        );
    }

    #[test]
    fn half_configured_access_is_left_out() {
        let c = Config {
            access_client_id: "id".into(),
            ..cfg("https://a", "t")
        };
        assert_eq!(
            decode(&connect_code(&c).unwrap()),
            json!({"url":"https://a","token":"t"})
        );
    }

    #[test]
    fn utf8_and_url_safe_alphabet() {
        // "?>>" style bytes encode to '/' / '+' in standard base64: must be '_' / '-'.
        let c = cfg("https://a", "トークン>>>???");
        let code = connect_code(&c).unwrap();
        assert_eq!(decode(&code)["token"], "トークン>>>???");
        assert!(!code.contains('+') && !code.contains('/') && !code.contains('='));
    }

    #[test]
    fn none_without_url_or_token() {
        assert!(connect_code(&cfg("", "t")).is_none());
        assert!(connect_code(&cfg("https://a", " ")).is_none());
        assert!(connect_code(&cfg("https://a", "REPLACE_WITH_CHIP_REMOTE_TOKEN")).is_none());
    }
}

#[cfg(test)]
mod android_compat {
    use super::*;
    use crate::Config;

    /// Same fixture as android/app/src/test/.../ConnectCodeTest.kt: both sides must agree
    /// byte for byte on the connect-code format.
    #[test]
    fn matches_android_fixture() {
        let cfg = Config {
            url: "https://x.example".into(),
            token: "t".into(),
            ..Config::default()
        };
        assert_eq!(
            connect_code(&cfg).as_deref(),
            Some("chipremote:eyJ1cmwiOiJodHRwczovL3guZXhhbXBsZSIsInRva2VuIjoidCJ9")
        );
    }
}
