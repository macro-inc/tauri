// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{
  borrow::Cow,
  collections::HashMap,
  sync::{Arc, Mutex, OnceLock},
};

use http::{Request, Response as HttpResponse, StatusCode, header::CONTENT_TYPE};
use tauri_utils::config::HeaderAddition;

use crate::{
  Manager, Runtime,
  manager::{AppManager, webview::PROXY_DEV_SERVER},
  webview::{UriSchemeProtocolHandler, WebResourceRequestHandler},
};

#[derive(Clone)]
struct CachedResponse {
  status: http::StatusCode,
  headers: http::HeaderMap,
  body: Vec<u8>,
}

/// Creates a URI scheme protocol handler for the `tauri://` custom protocol.
///
/// Accepts an application handle so custom protocols can delegate to this handler.
pub fn get<M: Manager<R>, R: Runtime>(
  manager: M,
  window_origin: &str,
  web_resource_request_handler: Option<Box<WebResourceRequestHandler>>,
) -> UriSchemeProtocolHandler {
  let manager = manager.manager_owned();
  let use_https = window_origin.starts_with("https");
  let url = {
    let mut url = manager.get_app_url(use_https).as_str().to_string();
    if url.ends_with('/') {
      url.pop();
    }
    url
  };

  let window_origin = window_origin.to_string();

  #[allow(unused_mut)]
  let mut client_builder = reqwest::ClientBuilder::new();
  if use_https {
    #[cfg(feature = "rustls-tls")]
    if rustls::crypto::CryptoProvider::get_default().is_none() {
      let _ = rustls::crypto::ring::default_provider().install_default();
    }

    // we can't load env vars at runtime, gotta embed them in the lib
    #[allow(unused_variables)]
    if let Some(cert_pem) = option_env!("TAURI_DEV_ROOT_CERTIFICATE") {
      #[cfg(any(
        feature = "native-tls",
        feature = "native-tls-vendored",
        feature = "rustls-tls"
      ))]
      {
        log::info!("adding dev server root certificate");
        let certificate = reqwest::Certificate::from_pem(cert_pem.as_bytes())
          .expect("failed to parse TAURI_DEV_ROOT_CERTIFICATE");
        client_builder = client_builder.tls_certs_merge([certificate]);
      }

      #[cfg(not(any(
        feature = "native-tls",
        feature = "native-tls-vendored",
        feature = "rustls-tls"
      )))]
      {
        log::warn!(
          "the dev root-certificate-path option was provided, but you must enable one of the following Tauri features in Cargo.toml: native-tls, native-tls-vendored, rustls-tls"
        );
      }
    } else {
      log::warn!(
        "loading HTTPS URL; you might need to provide a certificate via the `dev --root-certificate-path` option. You must enable one of the following Tauri features in Cargo.toml: native-tls, native-tls-vendored, rustls-tls"
      );
    }
  }
  let response_cache = Mutex::new(HashMap::new());

  let context = Arc::new(Context {
    manager,
    web_resource_request_handler,
    window_origin,
    client: LazyClient::new(client_builder),
    url,
    response_cache,
  });

  Box::new(move |_, request, responder| {
    let context = context.clone();
    crate::async_runtime::spawn(async move {
      match get_response(&context, request).await {
        Ok(response) => responder.respond(response),
        Err(e) => responder.respond(
          HttpResponse::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(CONTENT_TYPE, mime::TEXT_PLAIN.essence_str())
            .header("Access-Control-Allow-Origin", &context.window_origin)
            .body(e.to_string().into_bytes())
            .unwrap(),
        ),
      }
    });
  })
}

struct Context<R: Runtime> {
  manager: Arc<AppManager<R>>,
  window_origin: String,
  web_resource_request_handler: Option<Box<WebResourceRequestHandler>>,
  url: String,
  client: LazyClient,
  response_cache: Mutex<HashMap<String, CachedResponse>>,
}

/// Holds the configured [`reqwest::ClientBuilder`] until the [`reqwest::Client`] is actually
/// needed (only when proxying to the dev server), then builds and caches it for reuse.
struct LazyClient {
  builder: Mutex<Option<reqwest::ClientBuilder>>,
  client: OnceLock<reqwest::Client>,
}

impl LazyClient {
  fn new(builder: reqwest::ClientBuilder) -> Self {
    Self {
      builder: Mutex::new(Some(builder)),
      client: OnceLock::new(),
    }
  }

  /// Builds the client on first use and caches it for subsequent calls.
  fn get(&self) -> &reqwest::Client {
    self.client.get_or_init(|| {
      self
        .builder
        .lock()
        .unwrap()
        .take()
        .expect("HTTP client builder already consumed")
        .build()
        .unwrap()
    })
  }
}

async fn get_response<R: Runtime>(
  context: &Context<R>,
  request: Request<Vec<u8>>,
) -> Result<HttpResponse<Cow<'static, [u8]>>, Box<dyn std::error::Error>> {
  let Context {
    manager,
    web_resource_request_handler,
    window_origin,
    client,
    url,
    response_cache,
  } = context;

  let proxy_dev_server = PROXY_DEV_SERVER && manager.assets.iter().next().is_none();
  // the custom scheme URL format is defined by the runtime (see [`tauri_runtime::RuntimeHandle::custom_scheme_url`]),
  // so we must not assume any particular scheme/host layout here: the asset path is always the URI path,
  // no matter whether the request arrived as `tauri://localhost/index.html`, `http://tauri.localhost/index.html`
  // or any other format the runtime defines.
  let path = if proxy_dev_server {
    // keep the query string as we are going to proxy the request
    request
      .uri()
      .path_and_query()
      .map(|p| p.as_str().to_string())
      .unwrap_or_default()
  } else {
    // ignore query string and fragment
    request.uri().path().to_string()
  };

  #[allow(unused_mut)]
  let mut builder = HttpResponse::builder()
    .add_configured_headers(manager.config.app.security.headers.as_ref())
    .header("Access-Control-Allow-Origin", window_origin);

  let mut response = if proxy_dev_server {
    proxy_dev_request(client.get(), url, response_cache, path, builder, &request).await?
  } else {
    let use_https_scheme = request.uri().scheme() == Some(&http::uri::Scheme::HTTPS);
    let asset = manager.get_asset(path, use_https_scheme)?;
    builder = builder.header(CONTENT_TYPE, &asset.mime_type);
    if let Some(csp) = &asset.csp_header {
      builder = builder.header("Content-Security-Policy", csp);
    }
    builder.body(asset.bytes.into())?
  };

  if let Some(handler) = &web_resource_request_handler {
    handler(request, &mut response);
  }

  Ok(response)
}

async fn proxy_dev_request(
  client: &reqwest::Client,
  url: &str,
  response_cache: &Mutex<HashMap<String, CachedResponse>>,
  path: String,
  mut builder: http::response::Builder,
  request: &Request<Vec<u8>>,
) -> Result<HttpResponse<Cow<'static, [u8]>>, Box<dyn std::error::Error>> {
  let decoded_path = percent_encoding::percent_decode(path.as_bytes())
    .decode_utf8_lossy()
    .to_string();
  let url = format!(
    "{}/{}",
    url.trim_end_matches('/'),
    decoded_path.trim_start_matches('/')
  );

  let mut proxy_builder = client.request(request.method().clone(), &url);
  for (name, value) in request.headers() {
    proxy_builder = proxy_builder.header(name, value);
  }
  proxy_builder = proxy_builder.body(request.body().clone());

  let response = proxy_builder.send().await.map_err(|e|{
    let error_message = format!(
      "Failed to request {url}: {e}{}",
      if let Some(s) = e.status() {
        format!("status code: {}", s.as_u16())
      } else if cfg!(target_os = "ios") {
        ", did you grant local network permissions? That is required to reach the development server. Please grant the permission via the prompt or in `Settings > Privacy & Security > Local Network` and restart the app. See https://support.apple.com/en-us/102229 for more information.".to_string()
      } else {
        "".to_string()
      }
    );
    log::error!("{error_message}");
    error_message
  })?;

  let status = response.status();

  if status == http::StatusCode::NOT_MODIFIED
    && let Some(response) = response_cache.lock().unwrap().get(&url).cloned()
  {
    for (name, value) in &response.headers {
      builder = builder.header(name, value);
    }

    return Ok(builder.status(response.status).body(response.body.into())?);
  }

  let headers = response.headers().clone();
  let body = response.bytes().await?.to_vec();
  let response = CachedResponse {
    status,
    headers,
    body,
  };

  response_cache
    .lock()
    .unwrap()
    .insert(url.clone(), response.clone());

  for (name, value) in &response.headers {
    builder = builder.header(name, value);
  }

  builder
    .status(response.status)
    .body(response.body.into())
    .map_err(Into::into)
}

#[cfg(all(test, not(all(dev, mobile))))]
mod tests {
  use super::*;
  use crate::{
    Assets, UriSchemeResponder,
    test::{MockRuntime, mock_builder, mock_context, noop_assets},
  };
  use std::time::Duration;
  use tauri_utils::assets::{AssetKey, AssetsIter, CspHash};

  struct TestAssets;

  impl Assets<MockRuntime> for TestAssets {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
      (key.as_ref() == "/test.js").then_some(Cow::Borrowed(b"console.log('test');"))
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
      Box::new(std::iter::empty())
    }

    fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
      Box::new(std::iter::empty())
    }
  }

  fn request(handler: UriSchemeProtocolHandler, uri: &str) -> HttpResponse<Cow<'static, [u8]>> {
    let (tx, rx) = std::sync::mpsc::channel();
    handler(
      "main",
      Request::builder().uri(uri).body(Vec::new()).unwrap(),
      UriSchemeResponder(Box::new(move |response| tx.send(response).unwrap())),
    );
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
  }

  #[test]
  fn public_handler_accepts_app_handle_and_serves_assets() {
    let app = mock_builder().build(mock_context(TestAssets)).unwrap();
    let handler = crate::protocol::tauri::get(
      app.handle().clone(),
      "tauri://localhost",
      Some(Box::new(|request, response| {
        assert_eq!(request.uri().query(), Some("v=1"));
        response
          .headers_mut()
          .insert("x-custom", "yes".parse().unwrap());
      })),
    );
    let response = request(handler, "tauri://localhost/test.js?v=1");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
      response.headers()["Access-Control-Allow-Origin"],
      "tauri://localhost"
    );
    assert_eq!(response.headers()["x-custom"], "yes");
    assert_eq!(response.body().as_ref(), b"console.log('test');");
  }

  #[test]
  fn public_handler_returns_asset_errors_with_cors() {
    let app = mock_builder().build(mock_context(noop_assets())).unwrap();
    let handler = crate::protocol::tauri::get(app.handle().clone(), "tauri://localhost", None);
    let response = request(handler, "tauri://localhost/missing.js");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.headers()[CONTENT_TYPE], "text/plain");
    assert_eq!(
      response.headers()["Access-Control-Allow-Origin"],
      "tauri://localhost"
    );
    assert!(!response.body().is_empty());
  }
}
