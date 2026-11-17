//! Статика панели: интерфейс встроен в бинарник, поэтому разворачивать
//! рядом с сервером нечего. Отдельный модуль — потому что это единственное
//! место, которое знает имена файлов интерфейса.

use std::sync::Arc;

use axum::{
    http::header,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::App;

/// Все файлы интерфейса: страница, стили, скрипты, иконки и манифест.
pub(crate) fn router() -> Router<Arc<App>> {
    let mut router = Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }))
        .route("/md.js", get(|| async { asset("text/javascript; charset=utf-8", MD_JS) }))
        .route("/hl.js", get(|| async { asset("text/javascript; charset=utf-8", HL_JS) }))
        .route("/suggest.js", get(|| async { asset("text/javascript; charset=utf-8", SUGGEST_JS) }))
        // Service worker для push-уведомлений: лежит в корне, чтобы его область была весь сайт.
        .route("/sw.js", get(|| async { asset("text/javascript; charset=utf-8", SW_JS) }))
        .route("/style.css", get(|| async { asset("text/css; charset=utf-8", STYLE_CSS) }))
        .route("/icon.svg", get(|| async { asset("image/svg+xml", ICON_SVG) }))
        .route("/favicon.ico", get(|| async { asset_bytes("image/x-icon", FAVICON_ICO) }))
        .route("/apple-touch-icon.png", get(|| async { asset_bytes("image/png", APPLE_TOUCH_ICON) }))
        .route("/icon-192.png", get(|| async { asset_bytes("image/png", ICON_192) }))
        .route("/icon-512.png", get(|| async { asset_bytes("image/png", ICON_512) }))
        .route("/icon-maskable-512.png", get(|| async { asset_bytes("image/png", ICON_MASKABLE_512) }))
        // Значок уведомления для строки состояния Android: там картинка рисуется
        // силуэтом по альфе, поэтому нужен прозрачный фон без цветной плитки.
        .route("/badge.png", get(|| async { asset_bytes("image/png", BADGE) }))
        .route("/app.webmanifest", get(|| async { asset("application/manifest+json", MANIFEST) }));
    // Части панели — из таблицы: добавить файл значит дописать одну строку.
    for &(path, body) in SCRIPTS {
        router = router.route(path, get(move || async move { asset("text/javascript; charset=utf-8", body) }));
    }
    router
}

pub(crate) const INDEX_HTML: &str = include_str!("../static/index.html");
/// Части панели. Порядок тот же, что у тегов в `index.html`: каждая часть
/// видит объявления предыдущих, поэтому сборка не нужна — файлы отдаются как есть.
pub(crate) const SCRIPTS: &[(&str, &str)] = &[
    ("/app.core.js", include_str!("../static/app.core.js")),
    ("/app.boot.js", include_str!("../static/app.boot.js")),
    ("/app.sessions.js", include_str!("../static/app.sessions.js")),
    ("/app.notify.js", include_str!("../static/app.notify.js")),
    ("/app.push.js", include_str!("../static/app.push.js")),
    ("/app.feed.js", include_str!("../static/app.feed.js")),
    ("/app.favorites.js", include_str!("../static/app.favorites.js")),
    ("/app.paging.js", include_str!("../static/app.paging.js")),
    ("/app.prompts.js", include_str!("../static/app.prompts.js")),
    ("/app.launch-tree.js", include_str!("../static/app.launch-tree.js")),
    ("/app.launch.js", include_str!("../static/app.launch.js")),
    ("/app.commands.js", include_str!("../static/app.commands.js")),
    ("/app.composer.js", include_str!("../static/app.composer.js")),
    ("/app.shots.js", include_str!("../static/app.shots.js")),
    ("/app.main.js", include_str!("../static/app.main.js")),
];
pub(crate) const MD_JS: &str = include_str!("../static/md.js");
pub(crate) const HL_JS: &str = include_str!("../static/hl.js");
pub(crate) const SUGGEST_JS: &str = include_str!("../static/suggest.js");
pub(crate) const SW_JS: &str = include_str!("../static/sw.js");
pub(crate) const STYLE_CSS: &str = include_str!("../static/style.css");
pub(crate) const ICON_SVG: &str = include_str!("../static/icon.svg");
pub(crate) const MANIFEST: &str = include_str!("../static/app.webmanifest");
pub(crate) const FAVICON_ICO: &[u8] = include_bytes!("../static/favicon.ico");
pub(crate) const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../static/apple-touch-icon.png");
pub(crate) const ICON_192: &[u8] = include_bytes!("../static/icon-192.png");
pub(crate) const ICON_512: &[u8] = include_bytes!("../static/icon-512.png");
pub(crate) const ICON_MASKABLE_512: &[u8] = include_bytes!("../static/icon-maskable-512.png");
pub(crate) const BADGE: &[u8] = include_bytes!("../static/badge.png");
pub(crate) fn asset(content_type: &'static str, body: &'static str) -> Response {
    asset_bytes(content_type, body.as_bytes())
}

pub(crate) fn asset_bytes(content_type: &'static str, body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                // Картинки по ссылкам из ленты лежат на чужих сайтах, поэтому
                // img-src шире остального. Грузятся они только по клику на ссылку.
                "default-src 'self'; img-src 'self' data: blob: https: http:; frame-ancestors 'none'; base-uri 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Значок строки состояния Android рисуется силуэтом по альфе: если у
    /// картинки пропадёт прозрачность (её перерисуют на непрозрачном фоне),
    /// в панели состояния получится сплошной квадрат. Отсюда проверка на
    /// PNG с альфа-каналом и на то, что service worker ссылается на этот файл.
    #[test]
    fn notification_badge_has_alpha_and_is_used() {
        assert_eq!(&BADGE[..8], b"\x89PNG\r\n\x1a\n", "badge.png не PNG");
        // IHDR: сигнатура, длина, тип, ширина, высота, глубина, тип цвета.
        assert_eq!(BADGE[24], 8, "badge.png не 8 бит на канал");
        assert!(
            matches!(BADGE[25], 4 | 6),
            "у badge.png нет альфа-канала (тип цвета {})",
            BADGE[25]
        );
        assert!(SW_JS.contains("badge: '/badge.png'"), "service worker не использует /badge.png");
    }
}