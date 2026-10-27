//! Photocraft in the browser.
//!
//! Runs the same [`photocraft_ui_egui::PhotocraftApp`] as the desktop app through eframe's web
//! runner (wgpu: WebGPU where available, WebGL2 otherwise). Build with `trunk build --release`
//! from this directory; see `docs/development.md` ("Web build").
//!
//! Differences from the desktop app:
//! - no TCP control server (browsers can't listen on sockets);
//! - File → Open uses the browser file picker; bytes arrive asynchronously through
//!   `Services::inbox`;
//! - saving/exporting triggers a browser download;
//! - dropped files are read asynchronously by `web::WebShell` and delivered through the inbox.
//!
//! URL query flags: `?cpu` forces the CPU canvas path (same as `PHOTOCRAFT_CPU_CANVAS=1`);
//! `?webgl` forces the WebGL2 backend instead of WebGPU, while `?webgpu` overrides the
//! automatic Firefox WebGL2 compatibility path.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(any(target_arch = "wasm32", test))]
fn query_flag(query: &str, flag: &str) -> bool {
    query.trim_start_matches('?').split('&').any(|part| part.split_once('=').map_or(part, |(name, _)| name) == flag)
}

#[cfg(any(target_arch = "wasm32", test))]
fn use_webgl(query: &str, user_agent: &str) -> bool {
    query_flag(query, "webgl") || (!query_flag(query, "webgpu") && user_agent.contains("Firefox/"))
}

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("photocraft-web only runs in the browser: build it with `trunk build --release` in apps/photocraft-web");
}

#[cfg(test)]
mod tests {
    use super::{query_flag, use_webgl};

    /// A WebAssembly module that never downloads (or doesn't match the page) runs no Rust, so the
    /// page itself replaces the endless "Loading" with what may have gone wrong and a way to
    /// retry. The only hand-written script is the independent commerce shell; Trunk still owns
    /// the editor's generated loader.
    #[test]
    fn loading_page_offers_a_reload_when_startup_stalls() {
        let html = include_str!("../index.html");
        let (_, hint) = html.split_once("<p id=\"photocraft_stalled\">").expect("stalled-startup hint in the loading element");
        assert!(hint.contains("failed to") && hint.contains("<a href=\"\">Reload</a>"), "{hint}");
        assert!(html.contains("animation: photocraft-stalled"), "the hint appears after a delay");
        let scripts: Vec<_> = html.match_indices("<script").collect();
        assert_eq!(scripts.len(), 1, "only the commerce shell may be hand-written; Trunk injects the editor loader");
        assert!(html.contains("<script src=\"commerce.js\" defer></script>"));
        assert!(html.contains("rel=\"dns-prefetch\" href=\"https://widget.cloudpayments.ru\""));
        assert!(html.contains("rel=\"preconnect\" href=\"https://widget.cloudpayments.ru\" crossorigin"));
        assert!(html.contains("rel=\"preload\" href=\"https://widget.cloudpayments.ru/bundles/cloudpayments.js\" as=\"script\""));
    }

    #[test]
    fn page_uses_pixelle_title_and_first_party_svg_favicon() {
        let html = include_str!("../index.html");
        let favicon = include_str!("../../../assets/app-icon/pixelle.svg");
        assert!(html.contains("<title>Pixelle</title>"));
        assert!(html.contains("rel=\"icon\" type=\"image/svg+xml\" href=\"pixelle.svg\""));
        assert!(html.contains("data-trunk rel=\"copy-file\" href=\"../../assets/app-icon/pixelle.svg\""));
        assert!(favicon.contains("<title id=\"title\">Pixelle</title>"));
        for legacy in ["PhotoCraft", "ArtCraft", "Join us on Discord", "github.com/storytold"] {
            assert!(!html.contains(legacy), "legacy public branding remains: {legacy}");
        }
    }

    #[test]
    fn checkout_tokenizes_the_card_and_discloses_cancellation() {
        let commerce = include_str!("../commerce.js");
        assert!(commerce.contains("widget.pay(\"charge\""));
        assert!(commerce.contains("publicId: order.publicId"));
        assert!(commerce.contains("invoiceId: order.orderId"));
        assert!(commerce.contains("cloudPayments: { CustomerReceipt: cloudPaymentsReceipt(order.receipt) }"));
        assert!(!commerce.contains("recurrent: {"));
        assert!(!commerce.contains("widget.start("));
        assert!(!commerce.contains("script.crossOrigin"));
        assert!(commerce.contains("securitypolicyviolation"));
        assert!(commerce.contains("https://widget.cloudpayments.ru/"));
        assert!(commerce.contains("Платёжная форма заблокирована настройками безопасности страницы"));
        assert!(commerce.contains("1 615 ₽"));
        assert!(!commerce.contains("До 600 ₽"));
        assert!(commerce.contains("10 локальных проектов в пробном периоде"));
        assert!(commerce.contains("100 проектов с локальной обработкой после продления"));
        assert!(commerce.contains("href=\"/offer\""));
        assert!(commerce.contains("href=\"/rules\""));
        assert!(commerce.contains("href=\"/privacy\""));
        assert!(commerce.contains("Будущие платежи можно отменить"));
        assert!(!commerce.contains("фактическому расходу токенов"));
        assert!(commerce.contains("status.cancelAtPeriodEnd"));
        assert!(commerce.contains("Будущие списания отменены"));
    }

    #[test]
    fn paywall_authenticates_through_the_pixelle_identity_service() {
        let commerce = include_str!("../commerce.js");
        let web = include_str!("web.rs");
        assert!(commerce.contains("https://auth.neiro-platform.ru"));
        assert!(commerce.contains("/auth/exchange"));
        assert!(commerce.contains("if (error.status === 401) { showAuth(true); return; }"));
        assert!(web.contains("401 => open_commerce_auth_then_paywall()"));
        assert!(web.contains("open_commerce_method(\"showAuth\", Some(true))"));
        assert!(web.contains("403 => open_commerce_paywall()"));
    }

    #[test]
    fn checkout_errors_remain_inside_the_scrollable_paywall_body() {
        let commerce = include_str!("../commerce.js");
        let styles = include_str!("../commerce.css");
        assert!(commerce.contains("host.querySelector(\".paywall-body\") || host"));
        assert!(commerce.contains("errorHost.append(item)"));
        assert!(commerce.contains("if (!host.matches(\".px-commerce-auth\")) item.scrollIntoView({ block: \"nearest\" })"));
        assert!(styles.contains(".paywall-body {"));
        assert!(styles.contains("overflow-y: auto"));
        assert!(styles.contains(".paywall-modal .px-commerce-error"));
        assert!(!styles.contains(".paywall-modal > .px-commerce-error"));
    }

    #[test]
    fn firefox_uses_the_stable_webgl_backend_unless_webgpu_is_explicit() {
        let firefox = "Mozilla/5.0 Gecko/20100101 Firefox/145.0";
        assert!(use_webgl("", firefox));
        assert!(use_webgl("?cpu", firefox));
        assert!(!use_webgl("?webgpu", firefox));
        assert!(use_webgl("?webgpu&webgl", firefox));
        assert!(!use_webgl("", "Mozilla/5.0 Chrome/145.0"));
    }

    #[test]
    fn backend_flags_match_complete_query_parameter_names() {
        assert!(query_flag("?webgl", "webgl"));
        assert!(query_flag("?mode=edit&webgl=1", "webgl"));
        assert!(!query_flag("?notwebgl=1", "webgl"));
    }

    #[test]
    fn project_quota_is_applied_after_import_validation_without_making_local_editing_depend_on_billing_uptime() {
        let web = include_str!("web.rs");
        assert!(web.contains("photocraft_io::import(name, bytes)"));
        assert!(web.contains("meter_local_project(bytes).await"));
        assert!(web.contains("PRESET_LIBRARY_EXTS"));
        assert!(web.contains("authorize_opened_project(&name, &bytes).await"));
        assert!(web.contains("Err(ProjectAllowanceError::Denied(message)) => Err(message)"));
        assert!(web.contains("ProjectAllowanceError::Unavailable(message)"));
        assert!(web.contains("opening the local project without metering"));
        assert!(!web.contains("Ok(bytes) => meter_local_project(&bytes).await"));
    }
}
