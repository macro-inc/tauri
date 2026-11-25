// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::sync::Arc;

use cef::*;

use crate::webview::INITIAL_LOAD_URL;

wrap_load_handler! {
  pub struct TauriCefLoadHandler {
    on_page_load_handler: Option<Arc<tauri_runtime::webview::OnPageLoadHandler>>,
    frame_event_handler: Option<Arc<crate::FrameEventHandler>>,
    frame_navigation_state: crate::FrameNavigationState,
  }

  impl LoadHandler {
    fn on_loading_state_change(
      &self,
      browser: Option<&mut Browser>,
      is_loading: ::std::os::raw::c_int,
      _can_go_back: ::std::os::raw::c_int,
      _can_go_forward: ::std::os::raw::c_int,
    ) {
      if let Some(browser) = browser {
        let mut frame = browser.main_frame();
        crate::frame::emit_frame_event(
          &self.frame_event_handler,
          Some(browser),
          frame.as_mut(),
          crate::FrameEventKind::LoadingStateChanged { is_loading: is_loading != 0 },
        );
      }
    }

    fn on_load_start(
      &self,
      browser: Option<&mut Browser>,
      frame: Option<&mut Frame>,
      _transition_type: TransitionType,
    ) {
      let Some(frame) = frame else {
        return;
      };
      let raw_url = cef::CefString::from(&frame.url()).to_string();
      if let Ok(url) = url::Url::parse(&raw_url) {
        let is_main = frame.is_main() != 0;
        let browser_id = browser.as_ref().map(|browser| browser.identifier());
        // The frame observer sees every commit, the placeholder and browsers this
        // webview does not own included; it also binds the browser identity the
        // check below reads, so it runs first.
        crate::frame::emit_frame_event(
          &self.frame_event_handler,
          browser,
          Some(frame),
          crate::FrameEventKind::DocumentCommitted { url: url.clone() },
        );
        if let Some(handler) = &self.on_page_load_handler
          && reports_page_load(&self.frame_navigation_state, browser_id, is_main, &raw_url)
        {
          handler(url, tauri_runtime::webview::PageLoadEvent::Started);
        }
      }
    }

    fn on_load_error(
      &self,
      browser: Option<&mut Browser>,
      frame: Option<&mut Frame>,
      _error_code: Errorcode,
      _error_text: Option<&CefString>,
      failed_url: Option<&CefString>,
    ) {
      if let Some(failed_url) = failed_url
        && let Ok(url) = url::Url::parse(&failed_url.to_string())
      {
        crate::frame::emit_frame_event(
          &self.frame_event_handler,
          browser,
          frame,
          crate::FrameEventKind::NavigationFailed { url },
        );
      }
    }

    fn on_load_end(
      &self,
      browser: Option<&mut Browser>,
      frame: Option<&mut Frame>,
      _http_status_code: ::std::os::raw::c_int,
    ) {
      let Some(handler) = &self.on_page_load_handler else {
        return;
      };
      let Some(frame) = frame else {
        return;
      };

      let raw_url = cef::CefString::from(&frame.url()).to_string();
      let browser_id = browser.map(|browser| browser.identifier());
      if reports_page_load(
        &self.frame_navigation_state,
        browser_id,
        frame.is_main() != 0,
        &raw_url,
      ) && let Ok(url) = url::Url::parse(&raw_url)
      {
        handler(url, tauri_runtime::webview::PageLoadEvent::Finished);
      }
    }
  }
}

/// Whether a load of `url` in a frame of `browser_id` is the webview's own page
/// load, the only kind Tauri's `on_page_load` hears about.
///
/// Scoped the way the display and command handlers are: CEF routes browsers this
/// webview does not own through this very client (a DevTools window opened with
/// F12 or Inspect in a Chrome style browser), and their pages are not the
/// webview's. The webview is also created at [`INITIAL_LOAD_URL`], an internal
/// placeholder, before its real navigation; the display and request handlers skip
/// it, and so does this.
fn reports_page_load(
  frame_navigation_state: &crate::FrameNavigationState,
  browser_id: Option<i32>,
  is_main: bool,
  url: &str,
) -> bool {
  is_main
    && url != INITIAL_LOAD_URL
    && browser_id.is_some_and(|id| frame_navigation_state.has_browser_id(id))
}

#[cfg(test)]
mod tests {
  use super::*;

  const OWNED: i32 = 1;
  const DEVTOOLS: i32 = 2;
  const APP_URL: &str = "http://tauri.localhost/";

  /// A state bound to browser [`OWNED`], the way the frame observer binds it on
  /// the webview's first native notification.
  fn owned_state() -> crate::FrameNavigationState {
    let state = crate::FrameNavigationState::new();
    state.on_frame_event(&crate::FrameEvent {
      browser_id: OWNED,
      frame_id: "main".into(),
      is_main: true,
      kind: crate::FrameEventKind::Created,
    });
    state
  }

  #[test]
  fn reports_the_owned_browsers_main_frame() {
    assert!(reports_page_load(
      &owned_state(),
      Some(OWNED),
      true,
      APP_URL
    ));
  }

  #[test]
  fn skips_a_browser_the_webview_does_not_own() {
    let state = owned_state();
    assert!(!reports_page_load(
      &state,
      Some(DEVTOOLS),
      true,
      "devtools://devtools/bundled/devtools_app.html",
    ));
    assert!(!reports_page_load(&state, None, true, APP_URL));
  }

  #[test]
  fn skips_everything_before_the_browser_identity_is_bound() {
    let state = crate::FrameNavigationState::new();
    assert!(!reports_page_load(&state, Some(OWNED), true, APP_URL));
  }

  #[test]
  fn skips_the_initial_placeholder_page() {
    // The placeholder is a well-formed URL, so nothing downstream would drop it.
    assert!(url::Url::parse(INITIAL_LOAD_URL).is_ok());
    assert!(!reports_page_load(
      &owned_state(),
      Some(OWNED),
      true,
      INITIAL_LOAD_URL,
    ));
  }

  #[test]
  fn skips_subframes() {
    assert!(!reports_page_load(
      &owned_state(),
      Some(OWNED),
      false,
      APP_URL
    ));
  }
}
