//! A probe, not a test: which Chrome launch shape gets real search results.
//!
//! `#[ignore]`d and driven by environment variables so one binary can try
//! several shapes. It prints the page title and the first lines of text the
//! engine served, and asserts nothing, because what it exists to produce is an
//! observation for `docs/tools-web.html` rather than a pass.
//!
//! ```text
//! PROBE_URL=https://www.bing.com/search?q=ratatui+alternate+screen+rust \
//! PROBE_PROFILE=C:/some/persistent/dir PROBE_HEADFUL=1 PROBE_NO_AUTOMATION=1 \
//! PROBE_UA="Mozilla/5.0 ..." \
//! cargo test -p emma-tools-web --test search_probe -- --ignored --nocapture
//! ```

use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

#[tokio::test]
#[ignore = "a probe against the live network and a real Chrome"]
async fn which_launch_shape_gets_real_results() {
    let url = std::env::var("PROBE_URL")
        .unwrap_or_else(|_| "https://www.bing.com/search?q=ratatui+alternate+screen+rust".into());
    let mut cfg = BrowserConfig::builder()
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--mute-audio");
    if let Ok(dir) = std::env::var("PROBE_PROFILE") {
        cfg = cfg.user_data_dir(dir);
    }
    if std::env::var("PROBE_HEADFUL").is_ok() {
        cfg = cfg.with_head();
    }
    if std::env::var("PROBE_NO_AUTOMATION").is_ok() {
        cfg = cfg.arg("--disable-blink-features=AutomationControlled");
    }
    if let Ok(ua) = std::env::var("PROBE_UA") {
        cfg = cfg.arg(format!("--user-agent={ua}"));
    }
    let cfg = cfg.build().expect("a Chrome exists");
    let (mut browser, mut handler) = Browser::launch(cfg).await.expect("Chrome launches");
    let task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser.new_page(&url).await.expect("navigates");
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let title: String = page
        .evaluate("document.title")
        .await
        .ok()
        .and_then(|v| v.into_value().ok())
        .unwrap_or_default();
    let webdriver: String = page
        .evaluate("String(navigator.webdriver)")
        .await
        .ok()
        .and_then(|v| v.into_value().ok())
        .unwrap_or_default();
    let text: String = page
        .evaluate("document.body.innerText")
        .await
        .ok()
        .and_then(|v| v.into_value().ok())
        .unwrap_or_default();
    let final_url = page.url().await.ok().flatten().unwrap_or_default();

    println!("=== title: {title}");
    println!("=== final: {final_url}");
    println!("=== navigator.webdriver: {webdriver}");
    println!("=== first 30 non-empty lines:");
    for line in text.lines().filter(|l| !l.trim().is_empty()).take(30) {
        let line = line.trim();
        println!("    {}", line.chars().take(110).collect::<String>());
    }

    let _ = browser.close().await;
    let _ = browser.wait().await;
    task.abort();
}
