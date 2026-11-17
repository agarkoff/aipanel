//! Картинки из ленты: файл лежит на машине агента, поэтому сервер просит
//! его у агента и отдаёт браузеру как обычный HTTP-ответ.

use std::{sync::Arc, time::Duration};

use aipanel_proto::ServerMsg;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::{App, ImageWait};

/// Сколько ждать картинку от агента. Старый агент команду не знает и не ответит вовсе.
pub(crate) const IMAGE_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const MAX_IMAGE_WAITS: usize = 32;
// ---------------------------------------------------------------- картинки

#[derive(Deserialize)]
pub(crate) struct ImageQuery {
    agent: String,
    session: String,
    path: String,
}

/// Картинка с машины агента по пути из ленты сессии.
pub(crate) async fn image(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<ImageQuery>) -> Response {
    let Some(owner) = app.authed(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let path = q.path.trim().to_owned();
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) || q.session.len() > 256 {
        return image_error(StatusCode::BAD_REQUEST, "некорректный путь");
    }
    let (tx, rx) = oneshot::channel();
    let id = {
        let mut inner = app.lock();
        if inner.images.len() >= MAX_IMAGE_WAITS {
            return image_error(StatusCode::TOO_MANY_REQUESTS, "слишком много картинок загружается сразу");
        }
        let id = format!("img:{}", inner.id());
        let msg = ServerMsg::ReadImage { id: id.clone(), session: q.session, path };
        if !inner.to_agent(&owner, &q.agent, msg) {
            return image_error(StatusCode::BAD_GATEWAY, "агент не в сети");
        }
        inner.images.insert(id.clone(), ImageWait { owner, agent: q.agent, tx });
        id
    };
    let result = tokio::time::timeout(IMAGE_TIMEOUT, rx).await;
    app.lock().images.remove(&id);
    match result {
        Ok(Ok(Ok((mime, bytes)))) => image_response(&mime, bytes),
        Ok(Ok(Err(e))) => image_error(StatusCode::NOT_FOUND, &e),
        _ => image_error(
            StatusCode::GATEWAY_TIMEOUT,
            "агент не ответил: возможно, его нужно обновить до версии с показом картинок",
        ),
    }
}

pub(crate) fn image_response(mime: &str, bytes: Vec<u8>) -> Response {
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
    // SVG, открытый отдельной вкладкой, мог бы выполнить скрипт на домене панели:
    // sandbox и пустой default-src это запрещают.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; sandbox"),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("inline"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    resp
}

pub(crate) fn image_error(status: StatusCode, text: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        text.to_owned(),
    )
        .into_response()
}
