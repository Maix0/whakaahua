use std::{
    fmt::{Display, Write},
    io::Cursor,
    path::{Path as StdPath, PathBuf},
    str::FromStr,
    sync::Arc,
};

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, HeaderValue, Uri},
    response::{AppendHeaders, IntoResponse},
    Router,
};
use color_eyre::eyre::{eyre, Context, ContextCompat as _};
use image::ImageReader;
use reqwest::{header::CONTENT_TYPE, Url};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower_http::trace::TraceLayer;
use tracing::info;

type Result<T, E = AppError> = ::core::result::Result<T, E>;

#[derive(Clone)]
struct S {
    http: reqwest::Client,
    data_dir: Arc<StdPath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Resize {
    width: u32,
    height: u32,
}

impl<'de> serde::Deserialize<'de> for Resize {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Resize;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("{width}x{height}")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let Some((rhs, lhs)) = v.split_once("x") else {
                    return Err(E::custom("missing `x` separator"));
                };
                let (w, h) = (
                    rhs.parse::<u32>().map_err(E::custom)?,
                    lhs.parse::<u32>().map_err(E::custom)?,
                );
                Ok(Self::Value {
                    width: w,
                    height: h,
                })
            }

            fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str::<E>(v.as_str())
            }

            fn visit_bytes<E>(self, v: &[u8]) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str::<E>(core::str::from_utf8(v).map_err(E::custom)?)
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str::<E>(core::str::from_utf8(&v).map_err(E::custom)?)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

// Make our own error that wraps `anyhow::Error`.
struct AppError(color_eyre::Report);

impl AppError {
    fn wrap_err<T: Display + Send + Sync + 'static>(self, msg: T) -> Self {
        Self(self.0.wrap_err(msg))
    }
}
trait MyWrapErr {
    fn wrap_err<T: Display + Send + Sync + 'static>(self, msg: T) -> Self;
}

impl<O> MyWrapErr for Result<O, AppError> {
    fn wrap_err<T: Display + Send + Sync + 'static>(self, msg: T) -> Self {
        self.map_err(|e| e.wrap_err(msg))
    }
}

impl From<color_eyre::Report> for AppError {
    fn from(value: color_eyre::Report) -> Self {
        Self(value)
    }
}

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        eprintln!("an error occured: {:?}", self.0);
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

async fn download_image(state: &S, url: Url) -> Result<PathBuf> {
    let res = state
        .http
        .get(url.clone())
        .send()
        .await
        .wrap_err("Failed to build request")?;
    let body = res.bytes().await.wrap_err("Failed to get request")?;
    let file_path = {
        let mut p = std::path::PathBuf::from(&*state.data_dir);
        p.push({
            let a = url.path();
            let a = a.strip_prefix("/").unwrap_or(a);
            a.strip_suffix("/").unwrap_or(a)
        });
        p
    };

    tokio::fs::create_dir_all(&file_path.parent().ok_or_else(|| eyre!("no parent dir"))?)
        .await
        .wrap_err("Failed to create parent directory for path")?;

    let mut file = tokio::fs::File::options()
        .truncate(true)
        .write(true)
        .create_new(true)
        .open(&file_path)
        .await
        .wrap_err("failed to open file")?;

    file.write(&body)
        .await
        .wrap_err("failed to write to file")?;

    Ok(file_path)
}

async fn proxy_image(
    State(state): State<S>,
    Path((resize, paths)): Path<(Resize, String)>,
) -> Result<impl IntoResponse> {
    let src_path = {
        let mut p = PathBuf::from(&*state.data_dir);
        p.push(&paths);
        p
    };
    let out_path = {
        let mut p = PathBuf::from(&*state.data_dir);
        p.push(format!("{}x{}", resize.width, resize.height));
        p.push(&paths);
        p
    };
    dbg!(&src_path, &out_path);
    let headers = AppendHeaders([(CONTENT_TYPE, "image/png")]);
    if out_path.exists() {
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .open(&src_path)
            .await
            .wrap_err("Couldn't open file")?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .await
            .wrap_err("Couldn't read source file")?;
        return Ok((headers, buf));
    }

    if !src_path.exists() {
        let mut url = Url::parse("https://cdn.intra.42.fr").wrap_err("unable to parse domain")?;
        url.set_path(&paths);
        download_image(&state, url)
            .await
            .wrap_err("Failed to download image")?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .open(&src_path)
        .await
        .wrap_err("Couldn't open file")?;
    let mut src_buf = Vec::new();
    file.read_to_end(&mut src_buf)
        .await
        .wrap_err("Couldn't read source file")?;
    tokio::fs::create_dir_all(&out_path.parent().ok_or_else(|| eyre!("no parent dir"))?)
        .await
        .wrap_err("Failed to create parent directory for path")?;

    let mut out_file = tokio::fs::File::options()
        .create_new(true)
        .truncate(true)
        .write(true)
        .open(&out_path)
        .await
        .wrap_err("failed to popen out path")?;
    let src_img = ImageReader::new(Cursor::new(&src_buf))
        .with_guessed_format()
        .wrap_err("Failed to guess image format")?
        .decode()
        .wrap_err("Failed to decode image")?;

    let out_img = src_img.resize(
        resize.width,
        resize.height,
        image::imageops::FilterType::Lanczos3,
    );
    let mut out_buf = Vec::new();
    out_img
        .write_to(&mut Cursor::new(&mut out_buf), image::ImageFormat::Png)
        .wrap_err("failed to write to buffer")?;

    out_file
        .write(out_img.as_bytes())
        .await
        .wrap_err("Failed to write resized image")?;

    Ok((headers, out_buf))
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let port = std::env::var("P42_PORT")
        .map(|s| <u16 as FromStr>::from_str(&s))
        .wrap_err("P42_PORT env var is missing")?
        .wrap_err("P42_PORT can't be parsed as a port")?;
    let image_path = std::env::var_os("P42_DATA_DIR")
        .map(PathBuf::from)
        .wrap_err_with(|| "P42_DATA_DIR env var is missing")?;

    let state = S {
        http: reqwest::Client::new(),
        data_dir: Arc::from(image_path.as_path()),
    };

    let state = Router::new()
        .route("/proxy/{size}/{*path}", axum::routing::get(proxy_image))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    info!("Starting server on port {port} with data_dir = {image_path:?}");

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    axum::serve::serve(listener, state)
        .await
        .wrap_err("axum::Serve")?;

    Ok(())
}
