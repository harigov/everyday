//! One browser, one tab: doing an action to it, and looking at what came
//! of it.
//!
//! Everything this asks the page -- what an element is, whether something
//! covers it, which option a list has -- is asked in a JavaScript world of
//! this application's own (`Page.createIsolatedWorld`), which shares the
//! page's DOM and none of its scripts. A page can redefine
//! `Element.prototype.getAttribute` to say a password field is a search box;
//! it cannot do that here.
//!
//! What the person and the model are told the page *is* -- its address and
//! its site -- comes from the browser's own navigation history, never from
//! anything the page can write.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use base64::Engine as _;
use reqwest::Url;
use serde_json::{Value, json};

use super::action::{BrowserAction, Direction, Element, Facts, Target, Verdict, judge};
use super::cdp::Cdp;
use super::launch::{self, VIEWPORT};
use super::outline::{self, Refs};
use crate::webpage;

/// How long an action's page may take to finish loading before it is looked
/// at anyway.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(12);

/// The name of this application's own JavaScript world in each page.
const WORLD: &str = "everyday";

/// An action the gate let through, and what it saw when it did -- so the
/// action is refused if the page has changed underneath it since: a page that
/// navigates away while somebody reads the question, or swaps the field for
/// another, does not get what was approved for something else.
#[derive(Debug, Clone, Default)]
pub struct Approval {
    /// The address a page open was approved for.
    pub url: Option<String>,
    /// The site the page was on.
    pub site: Option<String>,
    /// The element the action was aimed at.
    pub node: Option<i64>,
}

impl Approval {
    pub fn open(url: &str) -> Self {
        Approval { url: Some(url.trim().to_string()), ..Approval::default() }
    }

    pub fn of(facts: &Facts) -> Self {
        Approval {
            url: None,
            site: Some(facts.site.clone()),
            node: facts.element.as_ref().map(|e| e.node),
        }
    }
}

/// The page after an action: what the model is shown.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    /// Where the browser is, by its own account.
    pub url: String,
    /// The site that address is on.
    pub site: String,
    /// The page's title -- the page's own words.
    pub title: String,
    /// The page, written out by [`outline::render`].
    pub page: String,
    /// Whether `page` was cut short.
    pub truncated: bool,
    /// What the model should be wary of about this page: see [`warnings`].
    pub warnings: Vec<String>,
    /// Dialogs the page put up since the last look, dismissed unanswered.
    pub dialogs: Vec<String>,
    /// Every address the page links to.
    pub links: Vec<String>,
    /// A sentence about the action itself, when it has one to say: the
    /// option a list was set to.
    pub done: Option<String>,
}

impl Observation {
    /// As the tool hands it to the model.
    pub fn to_json(&self) -> Value {
        let mut out = json!({
            "url": self.url,
            "site": self.site,
            "title": self.title,
            "page": self.page,
        });
        if self.truncated {
            out["truncated"] = Value::Bool(true);
        }
        if !self.warnings.is_empty() {
            out["warnings"] = json!(self.warnings);
        }
        if !self.dialogs.is_empty() {
            out["dialogs"] = json!(self.dialogs);
        }
        if let Some(done) = &self.done {
            out["done"] = Value::String(done.clone());
        }
        out
    }
}

/// One browser, started for one conversation.
pub struct Session {
    cdp: Arc<Cdp>,
    /// The tab's own CDP session.
    tab: String,
    child: Mutex<Option<std::process::Child>>,
    profile: PathBuf,
    refs: Mutex<Refs>,
    /// The document `refs` were given out in.
    document: Mutex<String>,
    dialogs: Arc<Mutex<Vec<String>>>,
    /// This application's own JavaScript world in the current document: the
    /// document's loader id, and the world's context. Made once per
    /// document rather than once per question.
    world: Mutex<Option<(String, i64)>>,
    /// The last page's address and links, which a later turn may open
    /// without asking -- see `super::seen`.
    seen: Mutex<String>,
    last_used: Mutex<Instant>,
    /// One action at a time.
    busy: tokio::sync::Mutex<()>,
}

impl Drop for Session {
    /// Ended on a thread of its own: killing a browser, waiting for it and
    /// deleting its profile is blocking work, and the last handle is often
    /// let go of on an async thread -- the one the proxy runs on, for one.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let profile = std::mem::take(&mut self.profile);
            let ended = std::thread::Builder::new()
                .name("browser-end".into())
                .spawn(move || launch::end(&mut child, &profile));
            if let Err(e) = ended {
                tracing::warn!(error = %e, "could not end a browser");
            }
        }
    }
}

/// The address of the page a browser is on, from its own history.
async fn location(cdp: &Cdp, tab: &str) -> Result<(String, String), String> {
    let history = cdp.call(Some(tab), "Page.getNavigationHistory", json!({})).await?;
    let index = history.get("currentIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let entry = history.get("entries").and_then(Value::as_array).and_then(|e| e.get(index));
    let text = |key: &str| {
        entry.and_then(|e| e.get(key)).and_then(Value::as_str).unwrap_or_default().to_string()
    };
    Ok((text("url"), text("title")))
}

/// The site an address is on, as its host -- in the ASCII form, so a name
/// spelled in look-alike letters shows as the `xn--` it really is.
pub fn site_of(url: &str) -> String {
    Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

/// What to be wary of about the page at `url`, in sentences for the model.
pub fn warnings(url: &str) -> Vec<String> {
    let Ok(url) = Url::parse(url) else { return Vec::new() };
    let mut out = Vec::new();
    let host = url.host_str().unwrap_or_default();
    if host.split('.').any(|label| label.starts_with("xn--")) {
        out.push(format!(
            "This site's name is spelled with letters from other alphabets (it is really \
             {host}). Names like that are used to imitate well-known sites: do not trust it to \
             be the site it looks like."
        ));
    }
    if webpage::literal_ip(&url).is_some() {
        out.push(
            "This site is a bare numeric address rather than a name. Be wary of what it claims \
             to be."
                .into(),
        );
    }
    if url.scheme() == "http" {
        out.push(
            "This page is not encrypted (http, not https): anything typed into it can be read \
             on the way."
                .into(),
        );
    }
    out
}

impl Session {
    /// Start a browser behind `proxy` and open a blank tab in it.
    pub async fn start(proxy: std::net::SocketAddr) -> Result<Arc<Session>, String> {
        let executable =
            launch::executable().ok_or("no Chrome or Chromium was found on this computer")?;
        let launched = tokio::task::spawn_blocking(move || launch::launch(&executable, proxy))
            .await
            .map_err(|e| e.to_string())??;
        let cdp = Cdp::over(launched.reader, launched.writer);
        let started = Self::attach(&cdp).await;
        let session = match started {
            Ok(tab) => Session {
                cdp: cdp.clone(),
                tab,
                child: Mutex::new(Some(launched.child)),
                profile: launched.profile,
                refs: Mutex::new(Refs::default()),
                document: Mutex::default(),
                dialogs: Arc::default(),
                world: Mutex::default(),
                seen: Mutex::default(),
                last_used: Mutex::new(Instant::now()),
                busy: tokio::sync::Mutex::new(()),
            },
            Err(e) => {
                let mut child = launched.child;
                launch::end(&mut child, &launched.profile);
                return Err(format!("the browser did not start: {e}"));
            }
        };
        let session = Arc::new(session);
        session.watch_dialogs();
        Ok(session)
    }

    /// Open the tab this session drives and set it up: no downloads, no file
    /// choosers, the fixed viewport.
    async fn attach(cdp: &Cdp) -> Result<String, String> {
        // The browser takes a moment to be ready to answer at all.
        let mut ready = Err(String::new());
        for _ in 0..50 {
            ready = cdp.call(None, "Browser.getVersion", json!({})).await;
            if ready.is_ok() || cdp.is_closed() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ready?;
        cdp.call(None, "Browser.setDownloadBehavior", json!({ "behavior": "deny" })).await?;
        let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" })).await?;
        let target_id = target.get("targetId").and_then(Value::as_str).ok_or("no tab")?;
        let attached = cdp
            .call(None, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }))
            .await?;
        let tab = attached.get("sessionId").and_then(Value::as_str).ok_or("no tab")?.to_string();
        let tab_ref = Some(tab.as_str());
        cdp.call(tab_ref, "Page.enable", json!({})).await?;
        cdp.call(tab_ref, "DOM.enable", json!({})).await?;
        cdp.call(tab_ref, "Accessibility.enable", json!({})).await?;
        // A page that asks for a file gets a dialog nobody answers.
        cdp.call(tab_ref, "Page.setInterceptFileChooserDialog", json!({ "enabled": true })).await?;
        let (width, height) = VIEWPORT;
        cdp.call(
            tab_ref,
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false }),
        )
        .await?;
        Ok(tab)
    }

    /// Dismiss every dialog the page puts up -- an alert, a confirm, a
    /// prompt, "leave this page?" -- unanswered, and keep what it said for
    /// the next look. A dialog left open would stop the page dead.
    fn watch_dialogs(self: &Arc<Self>) {
        let Some(mut events) = self.cdp.events() else { return };
        let cdp: Weak<Cdp> = Arc::downgrade(&self.cdp);
        let dialogs = self.dialogs.clone();
        let tab = self.tab.clone();
        super::background().spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                };
                if event.method != "Page.javascriptDialogOpening"
                    || event.session.as_deref() != Some(tab.as_str())
                {
                    continue;
                }
                let said = event.params.get("message").and_then(Value::as_str).unwrap_or_default();
                let kind = event.params.get("type").and_then(Value::as_str).unwrap_or("alert");
                {
                    let mut dialogs = dialogs.lock().unwrap();
                    if dialogs.len() < 5 {
                        dialogs.push(format!(
                            "The page showed a{} {kind} saying \u{201c}{}\u{201d}; it was \
                             dismissed without an answer.",
                            if kind.starts_with(['a', 'e', 'i', 'o', 'u']) { "n" } else { "" },
                            super::action::flat(said, 300)
                        ));
                    }
                }
                let Some(cdp) = cdp.upgrade() else { return };
                let _ = cdp
                    .call(Some(&tab), "Page.handleJavaScriptDialog", json!({ "accept": false }))
                    .await;
            }
        });
    }

    /// Where this browser's throwaway profile is.
    #[cfg(test)]
    pub fn profile(&self) -> PathBuf {
        self.profile.clone()
    }

    /// Whether the browser has gone.
    pub fn is_closed(&self) -> bool {
        self.cdp.is_closed()
    }

    /// How long since this session last did anything.
    pub fn idle(&self) -> Duration {
        self.last_used.lock().unwrap().elapsed()
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    /// The last page's address and every address it linked to.
    pub fn seen(&self) -> String {
        self.seen.lock().unwrap().clone()
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.cdp.call(Some(&self.tab), method, params).await
    }

    /// The address and title of the page, from the browser itself.
    pub async fn location(&self) -> Result<(String, String), String> {
        location(&self.cdp, &self.tab).await
    }

    /// The context of this application's own JavaScript world in the
    /// current document, made the first time it is needed there.
    async fn world(&self) -> Result<i64, String> {
        let (frame, document) = self.main_frame().await?;
        if let Some((made_in, context)) = self.world.lock().unwrap().as_ref()
            && *made_in == document
        {
            return Ok(*context);
        }
        let world = self
            .call(
                "Page.createIsolatedWorld",
                json!({ "frameId": frame, "worldName": WORLD, "grantUniveralAccess": false }),
            )
            .await?;
        let context = world.get("executionContextId").and_then(Value::as_i64).ok_or("no world")?;
        *self.world.lock().unwrap() = Some((document, context));
        Ok(context)
    }

    /// Forget the world, when the browser says its context has gone.
    fn lost_world(&self, error: &str) -> bool {
        let lost = error.contains("context") && !gone(error);
        if lost {
            self.world.lock().unwrap().take();
        }
        lost
    }

    /// Run `expression` in this application's own world, by value.
    async fn evaluate(&self, expression: &str) -> Result<Value, String> {
        let mut tries = 0;
        loop {
            let context = self.world().await?;
            let answer = self
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": expression, "contextId": context, "returnByValue": true }),
                )
                .await;
            match answer {
                Err(e) if tries == 0 && self.lost_world(&e) => tries += 1,
                answer => return value_of(&answer?),
            }
        }
    }

    /// Call `function` with `this` as `node`, in this application's own
    /// world, by value.
    async fn call_on(
        &self,
        node: i64,
        function: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, String> {
        let arguments: Vec<Value> = arguments.into_iter().map(|v| json!({ "value": v })).collect();
        let mut tries = 0;
        loop {
            let context = self.world().await?;
            let resolved = self
                .call(
                    "DOM.resolveNode",
                    json!({ "backendNodeId": node, "executionContextId": context }),
                )
                .await;
            let resolved = match resolved {
                Err(e) if tries == 0 && self.lost_world(&e) => {
                    tries += 1;
                    continue;
                }
                resolved => resolved?,
            };
            let object = resolved
                .pointer("/object/objectId")
                .and_then(Value::as_str)
                .ok_or("that element is gone")?;
            let answer = self
                .call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": function,
                        "objectId": object,
                        "arguments": arguments,
                        "returnByValue": true,
                    }),
                )
                .await?;
            return value_of(&answer);
        }
    }

    /// What `node` is, or `None` when it is no longer on the page.
    async fn describe(&self, node: i64) -> Result<Option<Element>, String> {
        match self.call_on(node, DESCRIBE, Vec::new()).await {
            Ok(described) => Ok(Element::from_described(node, &described)),
            Err(e) if gone(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The node a target names right now, and what it is.
    async fn resolve(&self, target: &Target) -> Result<Option<Element>, String> {
        let node = match target {
            Target::Ref(reference) => match self.refs.lock().unwrap().node(reference) {
                Some(node) => node,
                None => return Ok(None),
            },
            Target::Point { x, y } => {
                let found = self
                    .call(
                        "DOM.getNodeForLocation",
                        json!({ "x": x.round() as i64, "y": y.round() as i64,
                                "includeUserAgentShadowDOM": false,
                                "ignorePointerEventsNone": true }),
                    )
                    .await;
                match found.ok().and_then(|f| f.get("backendNodeId").and_then(Value::as_i64)) {
                    Some(node) => node,
                    None => return Ok(None),
                }
            }
        };
        self.describe(node).await
    }

    /// What the gate needs to know to judge `action` on this page.
    pub async fn facts(&self, action: &BrowserAction) -> Facts {
        let _busy = self.busy.lock().await;
        // Being judged is being used: a browser must not be closed as idle
        // while its next action waits for somebody's answer.
        self.touch();
        let url = self.location().await.map(|(url, _)| url).unwrap_or_default();
        let page_open = url.starts_with("http://") || url.starts_with("https://");
        let element = match action.target() {
            Some(target) if page_open => self.resolve(target).await.ok().flatten(),
            _ => None,
        };
        Facts { page_open, site: site_of(&url), element }
    }

    /// Do `action`, which the gate approved having seen what `approval`
    /// records, and look at the page afterwards.
    pub async fn perform(
        &self,
        action: &BrowserAction,
        approval: Approval,
    ) -> Result<Observation, String> {
        let _busy = self.busy.lock().await;
        self.touch();
        let mut done = None;
        match action {
            BrowserAction::Open { url } => {
                if approval.url.as_deref() != Some(url.trim()) {
                    return Err("That address was not the one approved.".into());
                }
                self.open(url).await?;
            }
            BrowserAction::Close => return Ok(Observation::default()),
            _ => {
                // The page as it is now, judged again: it must be the site
                // that was approved, the element that was approved, and still
                // something that may be done -- a field that has turned into a
                // password field since is refused here.
                let (url, _) = self.location().await?;
                let element = match action.target() {
                    Some(target) => self.resolve(target).await?,
                    None => None,
                };
                let facts = Facts {
                    page_open: url.starts_with("http://") || url.starts_with("https://"),
                    site: site_of(&url),
                    element,
                };
                if approval.site.as_deref() != Some(facts.site.as_str()) {
                    return Err(format!(
                        "The page moved to {} before this could be done, so it was not done. \
                         Look at the page again.",
                        if facts.site.is_empty() { "somewhere else" } else { facts.site.as_str() }
                    ));
                }
                if approval.node != facts.element.as_ref().map(|e| e.node) {
                    return Err("The page changed before this could be done, so it was not \
                                done. Look at the page again."
                        .into());
                }
                if let Verdict::Refuse(reason) = judge(action, &facts, false, false) {
                    return Err(reason);
                }
                // Listening before acting, so that a navigation the action
                // starts is not over before anybody is listening for it.
                let watching = self.cdp.events();
                done = self.act(action, facts.element.as_ref()).await?;
                self.settle_after(watching).await;
            }
        }
        let mut seen = self.observe_locked().await?;
        seen.done = done;
        Ok(seen)
    }

    /// Look at the page without doing anything to it.
    pub async fn observe(&self) -> Result<Observation, String> {
        let _busy = self.busy.lock().await;
        self.touch();
        self.observe_locked().await
    }

    async fn open(&self, url: &str) -> Result<(), String> {
        let parsed = webpage::parse(url)?;
        if let Some(ip) = webpage::literal_ip(&parsed)
            && crate::http::is_forbidden_address(ip, false)
        {
            return Err(
                "That address is on a private network, which this app will not open.".into()
            );
        }
        let answer = self.call("Page.navigate", json!({ "url": parsed.as_str() })).await?;
        if let Some(error) =
            answer.get("errorText").and_then(Value::as_str).filter(|e| !e.is_empty())
        {
            return Err(explain_navigation(error));
        }
        self.settle().await;
        Ok(())
    }

    /// Do one approved action to the page. What it has to say for itself,
    /// if anything.
    async fn act(
        &self,
        action: &BrowserAction,
        element: Option<&Element>,
    ) -> Result<Option<String>, String> {
        match action {
            BrowserAction::Click { target } => {
                let (x, y) = self.aim(target, element).await?;
                self.click_at(x, y).await?;
            }
            BrowserAction::Type { target, text, submit } => {
                match target {
                    Target::Ref(_) => {
                        let node = element.map(|e| e.node).ok_or("that field is gone")?;
                        let _ = self
                            .call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": node }))
                            .await;
                        // A text field gets its text set on the field itself,
                        // by the browser's own setter: nothing a page does
                        // with the focus can send it somewhere else.
                        match self.call_on(node, PUT_TEXT, vec![json!(text)]).await?.as_str() {
                            Some("set") => {}
                            Some("focused") => {
                                self.call("Input.insertText", json!({ "text": text })).await?;
                            }
                            _ => {
                                return Err("The page moved the focus to somewhere else, so \
                                            nothing was typed. Look at the page again."
                                    .into());
                            }
                        }
                    }
                    Target::Point { .. } => {
                        let (x, y) = self.aim(target, element).await?;
                        self.click_at(x, y).await?;
                        self.call("Input.insertText", json!({ "text": text })).await?;
                    }
                }
                if *submit {
                    self.press(super::action::Key::Enter).await?;
                }
            }
            BrowserAction::Select { option, .. } => {
                let node = element.map(|e| e.node).ok_or("that list is gone")?;
                let answer = self.call_on(node, SELECT_OPTION, vec![json!(option)]).await?;
                if let Some(error) = answer.get("error").and_then(Value::as_str) {
                    let options: Vec<&str> = answer
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    return Err(if options.is_empty() {
                        error.to_string()
                    } else {
                        format!("{error}. It offers: {}", options.join(" | "))
                    });
                }
                let chosen = answer.get("chosen").and_then(Value::as_str).unwrap_or(option);
                return Ok(Some(format!("Chose \u{201c}{chosen}\u{201d}.")));
            }
            BrowserAction::Press { key } => self.press(*key).await?,
            BrowserAction::Scroll { direction, target } => {
                let dy = f64::from(VIEWPORT.1)
                    * 0.8
                    * if *direction == Direction::Up { -1.0 } else { 1.0 };
                match (target, element) {
                    (Some(_), Some(element)) => {
                        self.call_on(
                            element.node,
                            "function (dy) { this.scrollBy(0, dy) }",
                            vec![json!(dy)],
                        )
                        .await?;
                    }
                    _ => {
                        let (width, height) = VIEWPORT;
                        self.call(
                            "Input.dispatchMouseEvent",
                            json!({ "type": "mouseWheel", "x": width / 2, "y": height / 2,
                                    "deltaX": 0, "deltaY": dy }),
                        )
                        .await?;
                    }
                }
            }
            BrowserAction::Back => {
                let history = self.call("Page.getNavigationHistory", json!({})).await?;
                let index =
                    history.get("currentIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
                let entry = index
                    .checked_sub(1)
                    .and_then(|i| history.get("entries")?.as_array()?.get(i)?.get("id")?.as_i64())
                    .ok_or("There is no page before this one.")?;
                self.call("Page.navigateToHistoryEntry", json!({ "entryId": entry })).await?;
            }
            BrowserAction::Look | BrowserAction::Open { .. } | BrowserAction::Close => {}
        }
        Ok(None)
    }

    /// Where to click to reach `target`: a point it was given, or the middle
    /// of the element, scrolled into view -- refused if something else is on
    /// top of it there, which is how a page tricks a click into landing on
    /// what it chose instead.
    async fn aim(&self, target: &Target, element: Option<&Element>) -> Result<(f64, f64), String> {
        if let Target::Point { x, y } = target {
            return Ok((*x, *y));
        }
        let node = element.map(|e| e.node).ok_or("that element is gone")?;
        let _ = self.call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": node })).await;
        let quads = self.call("DOM.getContentQuads", json!({ "backendNodeId": node })).await?;
        let (width, height) = (f64::from(VIEWPORT.0), f64::from(VIEWPORT.1));
        let point = quads
            .get("quads")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|q| {
                let q: Vec<f64> = q.as_array()?.iter().filter_map(Value::as_f64).collect();
                (q.len() == 8)
                    .then(|| ((q[0] + q[2] + q[4] + q[6]) / 4.0, (q[1] + q[3] + q[5] + q[7]) / 4.0))
            })
            .find(|(x, y)| (0.0..width).contains(x) && (0.0..height).contains(y))
            .ok_or("That element is not visible on the page, so it cannot be clicked.")?;
        let hit = self.call_on(node, HIT_TEST, vec![json!(point.0), json!(point.1)]).await?;
        match hit.as_str() {
            Some("ok") => Ok(point),
            Some(other) => Err(format!(
                "Something else on the page ({}) is covering that element, so the click was not \
                 made: a page that puts one thing over another is steering clicks. Look at the \
                 page again, or close what is in the way.",
                if other == "nothing" {
                    "nothing clickable".to_string()
                } else {
                    format!("<{other}>")
                }
            )),
            None => Err("could not tell what is under that element".into()),
        }
    }

    async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
        }
        Ok(())
    }

    async fn press(&self, key: super::action::Key) -> Result<(), String> {
        let (name, code, key_code, text) = key.cdp();
        let mut down = json!({
            "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": name, "code": code,
            "windowsVirtualKeyCode": key_code, "nativeVirtualKeyCode": key_code,
        });
        if let Some(text) = text {
            down["text"] = Value::String(text.to_string());
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": name, "code": code,
                    "windowsVirtualKeyCode": key_code, "nativeVirtualKeyCode": key_code }),
        )
        .await?;
        Ok(())
    }

    /// The id of the tab's top frame, and of the document in it.
    async fn main_frame(&self) -> Result<(String, String), String> {
        let tree = self.call("Page.getFrameTree", json!({})).await?;
        let text = |pointer: &str| {
            tree.pointer(pointer).and_then(Value::as_str).unwrap_or_default().to_string()
        };
        Ok((text("/frameTree/frame/id"), text("/frameTree/frame/loaderId")))
    }

    /// Wait for whatever an action started -- `events` was listening from
    /// before it -- to finish loading. A click that opens nothing settles
    /// in a moment; one that starts a navigation is waited for, since the
    /// page it leaves still says it is loaded until the new one commits.
    async fn settle_after(
        &self,
        events: Option<tokio::sync::broadcast::Receiver<super::cdp::Event>>,
    ) {
        if let Some(mut events) = events {
            let tab = self.tab.as_str();
            let main = self.main_frame().await.map(|(frame, _)| frame).unwrap_or_default();
            let in_main = |e: &super::cdp::Event| {
                e.session.as_deref() == Some(tab)
                    && e.params.get("frameId").and_then(Value::as_str) == Some(main.as_str())
            };
            let started = next_event(&mut events, Duration::from_millis(700), |e| {
                in_main(e)
                    && matches!(
                        e.method.as_str(),
                        "Page.frameStartedLoading"
                            | "Page.frameNavigated"
                            | "Page.navigatedWithinDocument"
                    )
            })
            .await;
            if started.as_deref().is_some_and(|m| m != "Page.navigatedWithinDocument") {
                next_event(&mut events, SETTLE_TIMEOUT, |e| {
                    e.session.as_deref() == Some(tab)
                        && (e.method == "Page.loadEventFired"
                            || (e.method == "Page.frameStoppedLoading" && in_main(e)))
                })
                .await;
            }
        }
        self.settle().await;
    }

    /// Wait for the document to say it has loaded, then a moment more for the
    /// page's own scripts to draw it.
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        while started.elapsed() < SETTLE_TIMEOUT {
            let state = self
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": "document.readyState", "returnByValue": true }),
                )
                .await;
            if state.ok().and_then(|s| value_of(&s).ok()).is_some_and(|v| v == "complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    async fn observe_locked(&self) -> Result<Observation, String> {
        let (url, title) = self.location().await?;
        // A new document starts its references again from e1: the old ones
        // name nothing that is still there.
        if let Ok((_, document)) = self.main_frame().await {
            let mut last = self.document.lock().unwrap();
            if *last != document {
                self.refs.lock().unwrap().clear();
                *last = document;
            }
        }
        let site = site_of(&url);
        let mut seen = Observation {
            warnings: warnings(&url),
            dialogs: std::mem::take(&mut *self.dialogs.lock().unwrap()),
            title: super::action::flat(&title, 200),
            ..Observation::default()
        };
        if url.starts_with("http://") || url.starts_with("https://") {
            // Text the person cannot see is marked hidden first, so the tree
            // leaves it out. Best effort: a page whose world will not start
            // is still read.
            let _ = self.evaluate(HIDE_INVISIBLE).await;
            let tree = self.call("Accessibility.getFullAXTree", json!({})).await?;
            let nodes = tree.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
            let written = outline::render(&nodes, &mut self.refs.lock().unwrap());
            let (page, truncated) = webpage::tidy(&written.text, webpage::MAX_PAGE_CHARS);
            seen.page = page;
            seen.truncated = truncated;
            seen.links = written.links;
        } else {
            self.refs.lock().unwrap().clear();
        }
        *self.seen.lock().unwrap() = std::iter::once(url.as_str())
            .chain(seen.links.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        seen.url = url;
        seen.site = site;
        Ok(seen)
    }

    /// The page as a picture, JPEG, at the viewport's size: what a model
    /// that works from screenshots is shown. See [`super::action::Target::Point`].
    pub async fn screenshot(&self) -> Result<Vec<u8>, String> {
        let _busy = self.busy.lock().await;
        let shot =
            self.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 70 })).await?;
        let data = shot.get("data").and_then(Value::as_str).ok_or("no picture")?;
        base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())
    }
}

/// The next event `wanted` picks out within `within`, by its method name.
async fn next_event(
    events: &mut tokio::sync::broadcast::Receiver<super::cdp::Event>,
    within: Duration,
    wanted: impl Fn(&super::cdp::Event) -> bool,
) -> Option<String> {
    use tokio::sync::broadcast::error::RecvError;
    let waiting = async {
        loop {
            match events.recv().await {
                Ok(event) if wanted(&event) => return Some(event.method),
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(within, waiting).await.ok().flatten()
}

/// A `Runtime.evaluate` or `callFunctionOn` answer's value, or the error the
/// script threw.
fn value_of(answer: &Value) -> Result<Value, String> {
    if let Some(thrown) = answer.get("exceptionDetails") {
        let text = thrown.pointer("/exception/description").or_else(|| thrown.get("text"));
        return Err(text.and_then(Value::as_str).unwrap_or("the page's script failed").to_string());
    }
    Ok(answer.pointer("/result/value").cloned().unwrap_or(Value::Null))
}

/// Whether a CDP error means the node is no longer there.
fn gone(error: &str) -> bool {
    error.contains("No node") || error.contains("gone") || error.contains("Could not find node")
}

/// A navigation's error in words.
fn explain_navigation(error: &str) -> String {
    match error {
        e if e.contains("TUNNEL_CONNECTION_FAILED") || e.contains("PROXY") => {
            "That site could not be reached: it does not exist, or it is on a private network, \
             which this app will not open."
                .into()
        }
        e if e.contains("NAME_NOT_RESOLVED") => "There is no site at that address.".into(),
        e if e.contains("TIMED_OUT") => "That site did not answer in time.".into(),
        e if e.contains("ABORTED") => {
            "That address did not open a page -- it may be a download, which this browser \
             refuses."
                .into()
        }
        e if e.contains("BLOCKED") => "That address is blocked.".into(),
        e => format!("The page did not open ({e})."),
    }
}

/// What an element is, as `action::Element::from_described` reads it.
const DESCRIBE: &str = r#"function () {
  const el = this.nodeType === 1 ? this : this.parentElement;
  if (!el) return null;
  const tag = el.tagName.toLowerCase();
  const type = (el.getAttribute('type') || '').toLowerCase();
  const textOf = (s) => (s || '').replace(/\s+/g, ' ').trim().slice(0, 160);
  let label = el.getAttribute('aria-label') || '';
  if (!label && el.labels && el.labels.length) label = el.labels[0].innerText;
  if (!label) {
    const by = el.getAttribute('aria-labelledby');
    if (by) label = by.split(/\s+/).map((id) => { const n = document.getElementById(id); return n ? n.innerText : ''; }).join(' ');
  }
  if (!label) label = el.getAttribute('placeholder') || el.getAttribute('title') || el.getAttribute('alt') || '';
  if (!label && !['input', 'textarea', 'select'].includes(tag)) label = el.innerText || el.textContent || '';
  if (!label && tag === 'input' && ['submit', 'button', 'reset'].includes(type)) label = el.value || '';
  const link = el.closest('a[href]');
  const textual = !['checkbox', 'radio', 'submit', 'button', 'reset', 'image', 'file', 'hidden', 'range', 'color'].includes(type);
  return {
    tag, type,
    role: (el.getAttribute('role') || '').toLowerCase(),
    label: textOf(label),
    name: el.getAttribute('name') || '',
    id: el.id || '',
    autocomplete: (el.getAttribute('autocomplete') || '').toLowerCase(),
    href: link ? String(link.href) : '',
    editable: !!(el.isContentEditable || tag === 'textarea' || (tag === 'input' && textual)) && !el.disabled && !el.readOnly,
  };
}"#;

/// Whether the point `(x, y)` lands on this element -- or inside it, or on
/// something it is inside -- rather than on something laid over it. Walks
/// into shadow roots, since `elementFromPoint` stops at their hosts.
const HIT_TEST: &str = r#"function (x, y) {
  let hit = document.elementFromPoint(x, y);
  while (hit && hit.shadowRoot) {
    const inner = hit.shadowRoot.elementFromPoint(x, y);
    if (!inner || inner === hit) break;
    hit = inner;
  }
  if (!hit) return 'nothing';
  const within = (a, b) => { for (let n = a; n; n = n.parentNode || n.host) { if (n === b) return true; } return false; };
  const self = this.nodeType === 1 ? this : this.parentElement;
  if (within(hit, self) || within(self, hit)) return 'ok';
  return (hit.tagName || 'something').toLowerCase();
}"#;

/// Put `text` into this field, replacing what is in it. An `<input>` or a
/// `<textarea>` has its value set through the browser's own setter (which a
/// page's framework sees as an edit, and which no focus trick can redirect)
/// and is answered `set`; anything else that is editable is focused, its
/// contents selected, and answered `focused`, for the browser to type into --
/// unless the focus has gone elsewhere by then, which is answered `moved`.
const PUT_TEXT: &str = r#"function (text) {
  const deepest = () => {
    let active = document.activeElement;
    while (active && active.shadowRoot && active.shadowRoot.activeElement) active = active.shadowRoot.activeElement;
    return active;
  };
  this.focus();
  if (this instanceof HTMLInputElement || this instanceof HTMLTextAreaElement) {
    const proto = this instanceof HTMLInputElement ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(this, text);
    this.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
    this.dispatchEvent(new Event('change', { bubbles: true }));
    return 'set';
  }
  if (deepest() !== this) return 'moved';
  const range = document.createRange();
  range.selectNodeContents(this);
  const selection = getSelection();
  selection.removeAllRanges();
  selection.addRange(range);
  return 'focused';
}"#;

/// Choose an option of a `<select>` by what it says, or by its value.
const SELECT_OPTION: &str = r#"function (wanted) {
  if (!(this instanceof HTMLSelectElement)) return { error: 'That is not a list to choose from; click it instead' };
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const w = norm(wanted);
  const options = Array.from(this.options);
  const hit = options.find((o) => norm(o.label) === w) || options.find((o) => norm(o.value) === w)
    || options.find((o) => norm(o.label).includes(w));
  if (!hit) return { error: 'No option says that', options: options.slice(0, 40).map((o) => o.label) };
  if (hit.disabled) return { error: 'That option cannot be chosen' };
  hit.selected = true;
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return { chosen: hit.label };
}"#;

/// Mark hidden every block of text the person could not see -- see the
/// outline module's doc. Long runs of text only, and never inside a control:
/// a screen-reader-only label on an icon button is a few words, and the
/// button needs it to have a name. What a page hides to talk to a model is
/// sentences.
///
/// Worked out afresh at every look: what this marked last time is unmarked
/// first, so a tab panel or a dialog that was hidden then and is shown now is
/// read. Only what this marked is unmarked -- an `aria-hidden` the page set
/// itself is the page's. `display: none` and `visibility: hidden` are left to
/// the accessibility tree, which already leaves them out; so is
/// `content-visibility: auto`, which is text further down the page, not
/// text hidden from anybody.
const HIDE_INVISIBLE: &str = r#"(() => {
  const MARK = 'data-everyday-hidden';
  for (const el of document.querySelectorAll('[' + MARK + ']')) {
    el.removeAttribute('aria-hidden');
    el.removeAttribute(MARK);
  }
  const MIN = 40;
  const root = document.body || document.documentElement;
  if (!root) return 0;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const looked = new Set();
  let hidden = 0, count = 0, node;
  const docW = Math.max(document.documentElement.scrollWidth, innerWidth);
  const docH = Math.max(document.documentElement.scrollHeight, innerHeight);
  const rgb = (s) => (s.match(/[\d.]+/g) || []).map(Number);
  // The colour behind an element, or null when it cannot be told: a picture
  // or a gradient behind the text is not a flat colour to compare with.
  const background = (el) => {
    for (let n = el; n && n.nodeType === 1; n = n.parentElement) {
      const style = getComputedStyle(n);
      if (style.backgroundImage && style.backgroundImage !== 'none') return null;
      const c = rgb(style.backgroundColor);
      if (c.length >= 3 && (c.length < 4 || c[3] > 0.5)) return c;
    }
    return [255, 255, 255];
  };
  const invisible = (el) => {
    if (el.checkVisibility && !el.checkVisibility({ opacityProperty: true, visibilityProperty: true })) return true;
    // Faded out, by the element or anything around it -- not every browser
    // version's `checkVisibility` looks at opacity.
    let opacity = 1;
    for (let n = el; n && n.nodeType === 1; n = n.parentElement) opacity *= parseFloat(getComputedStyle(n).opacity);
    if (opacity < 0.1) return true;
    const s = getComputedStyle(el);
    if (parseFloat(s.fontSize) < 4) return true;
    const colour = rgb(s.color);
    if (colour.length >= 4 && colour[3] < 0.1) return true;
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return true;
    if (r.right + scrollX <= 0 || r.bottom + scrollY <= 0 || r.left + scrollX >= docW || r.top + scrollY >= docH) return true;
    const bg = background(el);
    if (bg && colour.length >= 3 && Math.abs(colour[0] - bg[0]) + Math.abs(colour[1] - bg[1]) + Math.abs(colour[2] - bg[2]) < 24) return true;
    return false;
  };
  while ((node = walker.nextNode()) && count < 5000) {
    count++;
    if (!node.nodeValue || node.nodeValue.trim().length < MIN) continue;
    const el = node.parentElement;
    if (!el || looked.has(el)) continue;
    looked.add(el);
    if (el.closest('[aria-hidden="true"]')) continue;
    if (el.closest('a,button,input,select,textarea,label,summary,[role=button],[role=link],[contenteditable]')) continue;
    if (invisible(el)) { el.setAttribute('aria-hidden', 'true'); el.setAttribute(MARK, ''); hidden++; }
  }
  return hidden;
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_look_alike_name_a_bare_address_and_plain_http_are_called_out() {
        assert!(warnings("https://example.com/").is_empty());
        let spoof = warnings("https://xn--pypal-4ve.com/login");
        assert_eq!(spoof.len(), 1);
        assert!(spoof[0].contains("xn--pypal-4ve.com"), "{spoof:?}");
        // The browser reports the ASCII form, and so does `site_of`, even
        // for an address written in Unicode.
        assert_eq!(site_of("https://pаypal.com/"), "xn--pypal-4ve.com");
        assert_eq!(warnings("https://93.184.216.34/").len(), 1);
        assert_eq!(warnings("http://example.com/").len(), 1);
    }

    #[test]
    fn navigation_errors_are_sentences() {
        assert!(
            explain_navigation("net::ERR_TUNNEL_CONNECTION_FAILED").contains("private network")
        );
        assert!(explain_navigation("net::ERR_NAME_NOT_RESOLVED").contains("no site"));
    }
}
