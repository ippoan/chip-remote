//! 「スマホ接続用 QR を表示」: a small window with the connection code
//! ([`chip_core::connect_code`]) as a QR, for the phone app to scan.
//!
//! The page is generated in memory for every load by the `chipqr` URI scheme (config.json
//! is re-read, so the QR always matches the file). No network, no CDN, no script: the QR
//! is an inline SVG made by the `qrcode` crate. The page (and the QR) contains the token,
//! so nothing of it is logged, it is served only to the QR window and never cached.

use std::path::Path;

use chip_core::{connect_code, Config};
use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};
use tauri::http::{header, Response, StatusCode};
use tauri::{AppHandle, Manager as _, Url, WebviewUrl, WebviewWindowBuilder};

/// Window label (also the only webview the scheme answers).
pub const WINDOW_LABEL: &str = "qr";
/// URI scheme serving the page.
pub const SCHEME: &str = "chipqr";
pub const TITLE: &str = "スマホ接続用 QR";
pub const WARNING: &str = "この QR には token が含まれます。表示後は閉じてください";
pub const NO_CONFIG: &str = "設定がありません";

const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// Where the page is served. Windows (WebView2) exposes custom schemes as
/// `http://<scheme>.localhost/`, other platforms as `<scheme>://localhost/`.
pub fn page_url() -> Url {
    let s = if cfg!(windows) {
        format!("http://{SCHEME}.localhost/")
    } else {
        format!("{SCHEME}://localhost/")
    };
    Url::parse(&s).expect("static url")
}

/// The QR of `code` as an inline `<svg>` element (no XML prolog).
pub fn qr_svg(code: &str) -> Option<String> {
    let qr = QrCode::with_error_correction_level(code.as_bytes(), EcLevel::M).ok()?;
    let image = qr
        .render::<svg::Color>()
        .module_dimensions(8, 8)
        .quiet_zone(true)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    let start = image.find("<svg")?;
    Some(image[start..].to_string())
}

/// The whole page for `cfg` (None: config.json missing / unreadable / incomplete).
pub fn page_html(cfg: Option<&Config>) -> String {
    let body = match cfg.and_then(connect_code).and_then(|c| qr_svg(&c)) {
        Some(svg) => format!(r#"<div class="qr">{svg}</div><p class="warn">{WARNING}</p>"#),
        None => format!(
            r#"<p class="none">{NO_CONFIG}</p><p class="hint">トレイの「設定ファイルを開く」で url / token を設定してください</p>"#
        ),
    };
    format!(
        r#"<!doctype html>
<html lang="ja"><head><meta charset="utf-8"><title>{TITLE}</title>
<style>
html,body{{margin:0;height:100%;background:#fff;color:#111;font-family:"Yu Gothic UI","Meiryo",sans-serif}}
body{{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;padding:0 16px;box-sizing:border-box}}
.qr svg{{display:block;width:320px;height:320px}}
.warn{{margin:0;font-size:13px;text-align:center;color:#a33}}
.none{{margin:0;font-size:18px}}
.hint{{margin:0;font-size:13px;color:#555;text-align:center}}
</style></head><body>{body}</body></html>
"#
    )
}

/// Response for a request from webview `label`: the page for the QR window, 403 for
/// anything else.
pub fn respond(label: &str, config_path: &Path) -> Response<Vec<u8>> {
    if label != WINDOW_LABEL {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Vec::new())
            .expect("static response");
    }
    let cfg = Config::load(config_path).ok();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("Content-Security-Policy", CSP)
        .body(page_html(cfg.as_ref()).into_bytes())
        .expect("static headers")
}

/// Opens the QR window, or reloads (re-reads config.json) and focuses it when open.
pub fn show_window(app: &AppHandle) {
    tracing::info!("tray: showing the connection QR window");
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        let _ = w.navigate(page_url());
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built =
        WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::CustomProtocol(page_url()))
            .title(TITLE)
            .inner_size(400.0, 480.0)
            .center()
            .resizable(false)
            .maximizable(false)
            .focused(true)
            .build();
    if let Err(e) = built {
        tracing::warn!("tray: cannot open the QR window: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            url: "https://chip-remote.ippoan.org".into(),
            token: "secret-token".into(),
            access_client_id: "0123abcd.access".into(),
            access_client_secret: "access-secret-value".into(),
            ..Config::default()
        }
    }

    #[test]
    fn page_with_qr_and_warning_but_no_plain_secrets() {
        let html = page_html(Some(&cfg()));
        assert!(html.contains("<svg"), "{html}");
        assert!(!html.contains("<?xml"));
        assert!(html.contains(WARNING));
        assert!(html.contains(TITLE));
        assert!(!html.contains(NO_CONFIG));
        // The secrets are only in the QR modules, not as text.
        for s in [
            "secret-token",
            "access-secret-value",
            "0123abcd.access",
            "chipremote:",
        ] {
            assert!(!html.contains(s), "{s}");
        }
        // Self-contained: no external resources or scripts.
        assert!(!html.contains("<script"));
        assert!(!html.contains("src="));
        assert!(!html.contains("href="));
    }

    #[test]
    fn no_qr_without_url_or_token() {
        for c in [
            None,
            Some(Config {
                token: String::new(),
                ..cfg()
            }),
        ] {
            let html = page_html(c.as_ref());
            assert!(html.contains(NO_CONFIG));
            assert!(!html.contains("<svg"));
            assert!(!html.contains(WARNING));
        }
    }

    #[test]
    fn qr_fits_a_long_code() {
        let c = Config {
            token: "t".repeat(128),
            access_client_secret: "s".repeat(96),
            ..cfg()
        };
        let code = connect_code(&c).unwrap();
        assert!(qr_svg(&code).unwrap().starts_with("<svg"));
    }

    #[test]
    fn respond_serves_only_the_qr_window_and_reads_the_config() {
        let dir = std::env::temp_dir().join(format!(
            "chip-remote-qr-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        let other = respond("main", &p);
        assert_eq!(other.status(), StatusCode::FORBIDDEN);
        assert!(other.body().is_empty());

        let missing = respond(WINDOW_LABEL, &p);
        assert_eq!(missing.status(), StatusCode::OK);
        assert!(String::from_utf8_lossy(missing.body()).contains(NO_CONFIG));

        std::fs::write(&p, r#"{"url":"https://a","token":"secret-token"}"#).unwrap();
        let r = respond(WINDOW_LABEL, &p);
        let h = r.headers();
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        assert_eq!(h["content-security-policy"], CSP);
        assert!(h[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        assert!(String::from_utf8_lossy(r.body()).contains("<svg"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn url_matches_the_scheme() {
        let u = page_url();
        assert!(u.as_str().contains(SCHEME), "{u}");
    }
}
