//! Optional local compatibility API. All dictionary operations use the same query engine.
use crate::{
    data::store::{DictStore, QueryRequest, SearchMode},
    net::llm::{LlmConfig, TranslateMode},
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct App {
    db: PathBuf,
    llm: Option<LlmConfig>,
    permits: Arc<Semaphore>,
}

type ApiResult = Result<Json<serde_json::Value>, ApiError>;
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error":self.1}))).into_response()
    }
}

fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn dictionary(
    app: App,
    work: impl FnOnce(DictStore) -> anyhow::Result<serde_json::Value> + Send + 'static,
) -> ApiResult {
    let permit = app
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "请求过多".into()))?;
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(DictStore::open(&app.db)?)
    })
    .await
    .map_err(error)?
    .map_err(error)?;
    Ok(Json(result))
}

async fn word(State(app): State<App>, Path(word): Path<String>) -> ApiResult {
    valid(&word, 8)?;
    dictionary(app, move |store| {
        let result = store.query(&QueryRequest {
            query: word,
            mode: SearchMode::Auto,
            limit: 8,
        })?;
        Ok(serde_json::to_value(
            result
                .hits
                .into_iter()
                .flat_map(|hit| hit.cards)
                .collect::<Vec<_>>(),
        )?)
    })
    .await
}

#[derive(Deserialize)]
struct Search {
    q: String,
    #[serde(default = "default_mode")]
    mode: SearchMode,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_mode() -> SearchMode {
    SearchMode::Prefix
}
fn default_limit() -> usize {
    20
}
fn valid(query: &str, limit: usize) -> Result<(), ApiError> {
    let normalized = crate::data::store::normalize(query);
    if normalized.is_empty()
        || normalized.chars().count() > 256
        || normalized.chars().any(char::is_control)
        || !(1..=1000).contains(&limit)
    {
        Err(ApiError(
            StatusCode::BAD_REQUEST,
            "查询或 limit 无效".into(),
        ))
    } else {
        Ok(())
    }
}
async fn search(State(app): State<App>, Query(args): Query<Search>) -> ApiResult {
    valid(&args.q, args.limit)?;
    dictionary(app, move |store| {
        let result = store.query(&QueryRequest {
            query: args.q,
            mode: args.mode,
            limit: args.limit,
        })?;
        Ok(match args.mode {
            SearchMode::Contains | SearchMode::Prefix => {
                serde_json::to_value(result.hits.into_iter().map(|h| h.key).collect::<Vec<_>>())?
            }
            SearchMode::Fuzzy => serde_json::to_value(
                result
                    .suggestions
                    .into_iter()
                    .map(|s| (s.word, s.distance))
                    .collect::<Vec<_>>(),
            )?,
            _ => serde_json::to_value(result.hits)?,
        })
    })
    .await
}

#[derive(Deserialize)]
struct Random {
    #[serde(default = "default_limit")]
    n: usize,
}
async fn random(State(app): State<App>, Query(args): Query<Random>) -> ApiResult {
    valid("random", args.n)?;
    dictionary(app, move |store| {
        Ok(serde_json::to_value(store.random(args.n)?)?)
    })
    .await
}

#[derive(Deserialize)]
struct Translation {
    text: String,
    #[serde(default)]
    mode: Option<TranslateMode>,
    context: Option<String>,
    summary: Option<String>,
}
async fn translate(State(app): State<App>, Json(args): Json<Translation>) -> ApiResult {
    let cfg = app
        .llm
        .as_ref()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "翻译路由未启用".into()))?;
    if args.text.trim().is_empty() || args.text.chars().count() > 10000 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "文本须为 1–10000 字符".into(),
        ));
    }
    let _permit = app
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "请求过多".into()))?;
    let prompt = crate::net::llm::Prompt {
        mode: args.mode.unwrap_or_default(),
        context: args.context.as_deref(),
        summary: args.summary.as_deref(),
        ..Default::default()
    };
    let translated = crate::net::llm::translate_once(cfg, &prompt, &args.text)
        .await
        .map_err(error)?;
    Ok(Json(serde_json::json!({"translation":translated})))
}

pub async fn serve(addr: SocketAddr, db: PathBuf, llm: Option<LlmConfig>) -> anyhow::Result<()> {
    anyhow::ensure!(addr.ip().is_loopback(), "此版本只支持本机 loopback 地址");
    DictStore::open(&db)?;
    let app = Router::new()
        .route("/api/word/{word}", get(word))
        .route("/api/search", get(search))
        .route("/api/random", get(random))
        .route("/api/translate", post(translate))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(90),
        ))
        .with_state(App {
            db,
            llm,
            permits: Arc::new(Semaphore::new(8)),
        });
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("ZEDICT HTTP: http://{}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
