//! Browser tools without Playwright: a CDP browser (headless Google Chrome, or
//! Lightpanda via BROWSER_BIN) driven over the DevTools Protocol on a loopback
//! WebSocket, through one browser connection and a flat session on a new page target.
//!
//!   Chrome (fresh profile, own process group, --remote-debugging-port=0)
//!     └─ page target ◄── WebSocket ── Browser
//!          every request ─► Fetch.requestPaused ─► allowlisted host? continue : fail
//!
//! Each read polls `cancelled` every 100 ms, so a tool call stops promptly on cancel.
use super::{err, ToolError, CANCELLED, R};
use base64::Engine;
use serde_json::{json, Value as Json};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ACTION_TIMEOUT: Duration = Duration::from_millis(7000);

/// Exact host match over http(s) only (urlparse(url).hostname in hosts).
pub fn url_allowed(url: &str, hosts: &BTreeSet<String>) -> bool {
    host_of(url).is_some_and(|h| hosts.contains(&h))
}

pub fn host_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let host = if let Some(v6) = hostport.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

// ---------- minimal WebSocket client (RFC 6455, loopback only) ----------

struct Ws {
    s: TcpStream,
    buf: Vec<u8>,
}

impl Ws {
    fn connect(port: u16, path: &str) -> R<Ws> {
        let mut s = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(5),
        )
        .or_else(|e| err("ConnectionError", e))?;
        let key = base64::engine::general_purpose::STANDARD.encode(random16());
        write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
            .or_else(|e| err("ConnectionError", e))?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut buf = vec![];
        let mut tmp = [0u8; 4096];
        let end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let n = s
                .read(&mut tmp)
                .or_else(|e| err("ConnectionError", format!("WebSocket handshake: {e}")))?;
            if n == 0 {
                return err(
                    "ConnectionError",
                    "DevTools closed during the WebSocket handshake",
                );
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        if !buf.starts_with(b"HTTP/1.1 101") {
            return err("ConnectionError", "DevTools refused the WebSocket upgrade");
        }
        s.set_read_timeout(Some(Duration::from_millis(100))).ok();
        Ok(Ws {
            s,
            buf: buf[end..].to_vec(),
        })
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> R<()> {
        let mut f = vec![0x80 | opcode];
        let n = payload.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n < 65536 {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask: [u8; 4] = random16()[..4].try_into().unwrap();
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.s.write_all(&f).or_else(|e| err("ConnectionError", e))
    }

    /// Ensure `n` bytes are buffered, polling cancel and the deadline between reads.
    fn fill(&mut self, n: usize, deadline: Instant, cancelled: &dyn Fn() -> bool) -> R<()> {
        let mut tmp = [0u8; 65536];
        while self.buf.len() < n {
            if cancelled() {
                return Err(ToolError(CANCELLED.into()));
            }
            if Instant::now() >= deadline {
                return err(
                    "TimeoutError",
                    format!("Timeout {}ms exceeded.", ACTION_TIMEOUT.as_millis()),
                );
            }
            match self.s.read(&mut tmp) {
                Ok(0) => return err("ConnectionError", "Browser closed the DevTools connection"),
                Ok(k) => self.buf.extend_from_slice(&tmp[..k]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return err("ConnectionError", e),
            }
        }
        Ok(())
    }

    /// Next complete text message (fragments joined; pings answered).
    fn recv(&mut self, deadline: Instant, cancelled: &dyn Fn() -> bool) -> R<String> {
        let mut message = vec![];
        loop {
            self.fill(2, deadline, cancelled)?;
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let (mut head, mut len) = (2usize, (b1 & 0x7f) as u64);
            if len == 126 {
                self.fill(4, deadline, cancelled)?;
                len = u16::from_be_bytes([self.buf[2], self.buf[3]]) as u64;
                head = 4;
            } else if len == 127 {
                self.fill(10, deadline, cancelled)?;
                len = u64::from_be_bytes(self.buf[2..10].try_into().unwrap());
                head = 10;
            }
            let masked = b1 & 0x80 != 0;
            let total = head + if masked { 4 } else { 0 } + len as usize;
            self.fill(total, deadline, cancelled)?;
            let frame: Vec<u8> = self.buf.drain(..total).collect();
            let mut payload = frame[total - len as usize..].to_vec();
            if masked {
                let m = &frame[head..head + 4];
                payload
                    .iter_mut()
                    .enumerate()
                    .for_each(|(i, b)| *b ^= m[i % 4]);
            }
            match b0 & 0x0f {
                0x9 => self.send(0xA, &payload)?,
                0xA => {}
                0x8 => return err("ConnectionError", "Browser closed the DevTools connection"),
                _ => {
                    message.extend_from_slice(&payload);
                    if b0 & 0x80 != 0 {
                        return Ok(String::from_utf8_lossy(&message).into_owned());
                    }
                }
            }
        }
    }
}

/// GET a DevTools JSON endpoint. Chrome keeps the connection open, so read by Content-Length.
fn devtools_get(port: u16, path: &str) -> R<Json> {
    let mut s = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(5),
    )
    .or_else(|e| err("ConnectionError", e))?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")
        .or_else(|e| err("ConnectionError", e))?;
    let mut buf = vec![];
    let mut tmp = [0u8; 8192];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= i + 4 + len {
                return serde_json::from_slice(&buf[i + 4..i + 4 + len])
                    .or_else(|e| err("BrowserError", e));
            }
        }
        let n = s
            .read(&mut tmp)
            .or_else(|e| err("ConnectionError", format!("DevTools {path}: {e}")))?;
        if n == 0 {
            return err("ConnectionError", "DevTools closed the connection");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    let _ = fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    b
}

// ---------- Chrome over CDP ----------

pub struct Browser {
    _chrome: KillGuard,
    ws: Ws,
    next_id: u64,
    hosts: BTreeSet<String>,
    main_frame: String,
    nav_started: bool,
    dom_ready: bool,
    /// Flat CDP session of the page target; page-domain commands carry it.
    session: String,
    pub engine: &'static str,
}

/// BROWSER_BIN (any CDP browser, e.g. Lightpanda), else CHROME_BIN, else Google Chrome.
pub fn chrome_binary() -> PathBuf {
    std::env::var("BROWSER_BIN")
        .or_else(|_| std::env::var("CHROME_BIN"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into())
}

fn is_lightpanda(bin: &Path) -> bool {
    bin.file_name()
        .is_some_and(|n| n.to_string_lossy().to_lowercase().contains("lightpanda"))
}

impl Browser {
    pub fn launch(
        run_dir: &Path,
        hosts: BTreeSet<String>,
        cancelled: &dyn Fn() -> bool,
    ) -> R<Browser> {
        let profile = run_dir.join(format!(".chrome-profile-{}", hex::encode(&random16()[..4])));
        fs::create_dir_all(&profile).or_else(|e| super::io_err(e, &profile))?;
        let log =
            fs::File::create(run_dir.join("chrome.log")).or_else(|e| super::io_err(e, run_dir))?;
        let bin = chrome_binary();
        let lightpanda = is_lightpanda(&bin);
        let lp_port = if lightpanda {
            std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .map(|a| a.port())
                .or_else(|e| err("OSError", e))?
        } else {
            0
        };
        let mut cmd = Command::new(&bin);
        if lightpanda {
            // Lightpanda: one CDP server process, no renderer tree, no profile on disk.
            cmd.args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &lp_port.to_string(),
            ])
            .env("LIGHTPANDA_DISABLE_TELEMETRY", "true");
        } else {
            cmd.arg("--headless=new")
                .arg("--remote-debugging-port=0")
                .arg(format!("--user-data-dir={}", profile.display()))
                .args([
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--disable-extensions",
                    "--disable-background-networking",
                    "--disable-sync",
                    "--disable-component-update",
                    "--disable-default-apps",
                    "--metrics-recording-only",
                    "--password-store=basic",
                    "--use-mock-keychain",
                    "--window-size=1000,700",
                    // Memory: one renderer, no GPU process work, no background features.
                    "--disable-gpu",
                    "--disable-software-rasterizer",
                    "--renderer-process-limit=1",
                    "--disable-features=Translate,OptimizationHints,MediaRouter,BackForwardCache",
                    "about:blank",
                ]);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().or_else(|e| {
            err(
                "BrowserError",
                format!("could not start the browser ({}): {e}", bin.display()),
            )
        })?;
        // From here on, any early return drops the guard, which kills the browser.
        let port_file = profile.join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_secs(20);
        let chrome = KillGuard(Some(child), profile.clone());
        let (port, version) = loop {
            let port = if lightpanda {
                Some(lp_port)
            } else {
                fs::read_to_string(&port_file)
                    .ok()
                    .and_then(|t| t.lines().next().and_then(|l| l.trim().parse().ok()))
            };
            if let Some(v) = port.and_then(|p| devtools_get(p, "/json/version").ok()) {
                break (port.unwrap(), v);
            }
            if cancelled() {
                return Err(ToolError(CANCELLED.into()));
            }
            if Instant::now() >= deadline {
                return err(
                    "TimeoutError",
                    "The browser did not open its DevTools port within 20 seconds",
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let ws_url = version["webSocketDebuggerUrl"]
            .as_str()
            .ok_or_else(|| ToolError("BrowserError: no browser WebSocket endpoint".into()))?;
        let path = ws_url
            .splitn(4, '/')
            .nth(3)
            .map(|p| format!("/{p}"))
            .unwrap_or_else(|| "/".into());
        let ws = Ws::connect(port, &path)?;
        let mut b = Browser {
            _chrome: chrome,
            ws,
            next_id: 0,
            hosts,
            main_frame: String::new(),
            nav_started: false,
            dom_ready: false,
            session: String::new(),
            engine: if lightpanda { "lightpanda" } else { "chrome" },
        };
        let target = b.call(
            "Target.createTarget",
            json!({"url": "about:blank"}),
            cancelled,
        )?;
        let target_id = target["targetId"].as_str().unwrap_or_default().to_string();
        let attached = b.call(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            cancelled,
        )?;
        b.session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| ToolError("BrowserError: could not attach to the page".into()))?
            .to_string();
        b.main_frame = target_id;
        b.call("Page.enable", json!({}), cancelled)?;
        // The host allowlist depends on request interception; never browse without it.
        b.call(
            "Fetch.enable",
            json!({"patterns": [{"urlPattern": "*"}]}),
            cancelled,
        )?;
        let _ = b.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": 1000, "height": 700, "deviceScaleFactor": 1, "mobile": false}),
            cancelled,
        );
        if let Ok(tree) = b.call("Page.getFrameTree", json!({}), cancelled) {
            if let Some(id) = tree.pointer("/frameTree/frame/id").and_then(Json::as_str) {
                b.main_frame = id.to_string();
            }
        }
        Ok(b)
    }

    fn handle_event(&mut self, m: &Json) -> R<()> {
        let p = &m["params"];
        match m["method"].as_str().unwrap_or_default() {
            "Fetch.requestPaused" => {
                let id = p["requestId"].clone();
                let allowed = p
                    .pointer("/request/url")
                    .and_then(Json::as_str)
                    .is_some_and(|u| url_allowed(u, &self.hosts));
                let (method, params) = if allowed {
                    ("Fetch.continueRequest", json!({"requestId": id}))
                } else {
                    (
                        "Fetch.failRequest",
                        json!({"requestId": id, "errorReason": "BlockedByClient"}),
                    )
                };
                self.next_id += 1;
                let mut msg = json!({"id": self.next_id, "method": method, "params": params});
                if let Some(sid) = m.get("sessionId") {
                    msg["sessionId"] = sid.clone();
                }
                self.ws.send(0x1, msg.to_string().as_bytes())?;
            }
            "Page.frameStartedLoading" | "Page.frameRequestedNavigation"
                if p["frameId"] == json!(self.main_frame) =>
            {
                self.nav_started = true
            }
            "Page.domContentEventFired" => self.dom_ready = true,
            _ => {}
        }
        Ok(())
    }

    fn call(&mut self, method: &str, params: Json, cancelled: &dyn Fn() -> bool) -> R<Json> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if !method.starts_with("Target.") && !self.session.is_empty() {
            msg["sessionId"] = json!(self.session);
        }
        self.ws.send(0x1, msg.to_string().as_bytes())?;
        let deadline = Instant::now() + ACTION_TIMEOUT;
        loop {
            let m: Json =
                serde_json::from_str(&self.ws.recv(deadline, cancelled)?).unwrap_or(Json::Null);
            if m["id"] == json!(id) {
                if let Some(e) = m.get("error") {
                    return err(
                        "BrowserError",
                        format!(
                            "{method}: {}",
                            e["message"].as_str().unwrap_or("protocol error")
                        ),
                    );
                }
                return Ok(m["result"].clone());
            }
            if m.get("method").is_some() {
                self.handle_event(&m)?;
            }
        }
    }

    /// Process events (e.g. intercepted requests) until `flag` is set or `wait` elapses.
    fn pump_until(
        &mut self,
        wait: Duration,
        flag: fn(&Browser) -> bool,
        cancelled: &dyn Fn() -> bool,
    ) -> R<bool> {
        let deadline = Instant::now() + wait;
        while !flag(self) {
            match self.ws.recv(deadline, cancelled) {
                Ok(t) => {
                    let m: Json = serde_json::from_str(&t).unwrap_or(Json::Null);
                    if m.get("method").is_some() {
                        self.handle_event(&m)?;
                    }
                }
                Err(ToolError(e)) if e.starts_with("TimeoutError") => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    fn eval(&mut self, expr: &str, cancelled: &dyn Fn() -> bool) -> R<Json> {
        let r = self.call(
            "Runtime.evaluate",
            json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
            cancelled,
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            return err(
                "Error",
                ex.pointer("/exception/description")
                    .and_then(Json::as_str)
                    .unwrap_or("page script failed"),
            );
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Json::Null))
    }

    pub fn goto(&mut self, url: &str, cancelled: &dyn Fn() -> bool) -> R<()> {
        self.dom_ready = false;
        let r = self.call("Page.navigate", json!({"url": url}), cancelled)?;
        if let Some(e) = r.get("errorText").and_then(Json::as_str) {
            return err("Error", format!("Page.goto: {e} at {url}"));
        }
        if r.get("loaderId").is_some()
            && !self.pump_until(ACTION_TIMEOUT, |b| b.dom_ready, cancelled)?
        {
            return err(
                "TimeoutError",
                format!(
                    "Page.goto: Timeout {}ms exceeded.",
                    ACTION_TIMEOUT.as_millis()
                ),
            );
        }
        Ok(())
    }

    pub fn observe(&mut self, cancelled: &dyn Fn() -> bool) -> R<Json> {
        // Page text is capped at 7000 characters. When the URL has a #fragment that names an element,
        // the window starts at that element, so section links (Wikipedia's "Accident") reach the section.
        self.eval(r#"(() => ({url: location.href, text: (() => {
    if (!document.body) return '';
    const all = document.body.innerText;
    let start = 0;
    const id = location.hash ? decodeURIComponent(location.hash.slice(1)) : '';
    const el = id ? document.getElementById(id) : null;
    if (el) {
      const head = (el.innerText || el.textContent || '').trim().slice(0, 60);
      const at = head ? all.indexOf(head) : -1;
      if (at > 0) start = at;
    }
    return (start ? '[from #' + id + '] ' : '') + all.slice(start, start + 7000);
  })(),
  controls: Array.from(document.querySelectorAll('input,button,a,select,textarea')).filter(e => e.getClientRects().length)
    .map(e => ({tag: e.tagName, id: e.id ?? null, name: e.name ?? null, type: e.type ?? null, text: e.innerText ?? null,
                placeholder: e.placeholder ?? null, href: e.getAttribute('href')})).slice(0, 60)}))()"#, cancelled)
    }

    /// Wait (up to 7 s) for exactly one visible, enabled match; run `action` JS on it.
    fn locate(&mut self, selector: &str, action: &str, cancelled: &dyn Fn() -> bool) -> R<Json> {
        let expr = format!(
            r#"((sel) => {{
  let els; try {{ els = document.querySelectorAll(sel); }} catch (e) {{ return {{error: 'Invalid selector: ' + e.message}}; }}
  if (els.length === 0) return {{wait: 'no element matches the selector'}};
  if (els.length > 1) return {{error: 'strict mode violation: selector resolved to ' + els.length + ' elements'}};
  const el = els[0];
  el.scrollIntoView({{block: 'center', inline: 'center', behavior: 'instant'}});
  const r = el.getBoundingClientRect(), st = getComputedStyle(el);
  if (!r.width || !r.height || st.visibility === 'hidden') return {{wait: 'element is not visible'}};
  if (el.disabled) return {{wait: 'element is not enabled'}};
  const x = r.x + r.width / 2, y = r.y + r.height / 2;
  {action}
}})({})"#,
            serde_json::to_string(selector).unwrap_or_default()
        );
        let deadline = Instant::now() + ACTION_TIMEOUT;
        loop {
            let v = self.eval(&expr, cancelled)?;
            if let Some(e) = v.get("error").and_then(Json::as_str) {
                return err("Error", e);
            }
            match v.get("wait").and_then(Json::as_str) {
                None => return Ok(v),
                Some(w) if Instant::now() >= deadline => {
                    return err(
                        "TimeoutError",
                        format!("Timeout {}ms exceeded: {w}", ACTION_TIMEOUT.as_millis()),
                    )
                }
                Some(_) => {
                    self.pump_until(Duration::from_millis(100), |_| false, cancelled)?;
                }
            }
        }
    }

    pub fn click(&mut self, selector: &str, cancelled: &dyn Fn() -> bool) -> R<()> {
        let at = self.locate(
            selector,
            "const hit = document.elementFromPoint(x, y);
  if (!(hit === el || el.contains(hit))) return {wait: 'element is covered by another element'};
  return {x, y};",
            cancelled,
        )?;
        let (x, y) = (
            at["x"].as_f64().unwrap_or(0.0),
            at["y"].as_f64().unwrap_or(0.0),
        );
        self.nav_started = false;
        self.dom_ready = false;
        for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
            let mut p = json!({"type": kind, "x": x, "y": y, "buttons": buttons});
            if kind != "mouseMoved" {
                p["button"] = json!("left");
                p["clickCount"] = json!(1);
            }
            self.call("Input.dispatchMouseEvent", p, cancelled)?;
        }
        // If the click started a navigation, wait for the new document like Playwright does.
        self.pump_until(Duration::from_millis(300), |b| b.nav_started, cancelled)?;
        if self.nav_started && !self.pump_until(ACTION_TIMEOUT, |b| b.dom_ready, cancelled)? {
            return err(
                "TimeoutError",
                "Navigation after click did not finish within 7000ms",
            );
        }
        Ok(())
    }

    pub fn fill(&mut self, selector: &str, text: &str, cancelled: &dyn Fn() -> bool) -> R<()> {
        self.locate(selector, "const t = el.tagName, bad = ['checkbox','radio','button','submit','reset','file','image','hidden','range','color'];
  if (!(el.isContentEditable || t === 'TEXTAREA' || (t === 'INPUT' && !bad.includes(el.type))))
    return {error: 'Element is not an <input>, <textarea> or [contenteditable] element'};
  if (el.readOnly) return {wait: 'element is not editable'};
  el.focus();
  if (el.select) el.select(); else { const r = document.createRange(); r.selectNodeContents(el); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
  return {ok: true};", cancelled)?;
        if text.is_empty() {
            self.eval("document.execCommand('delete')", cancelled)?;
        } else {
            self.call("Input.insertText", json!({"text": text}), cancelled)?;
        }
        Ok(())
    }

    pub fn screenshot(&mut self, path: &Path, cancelled: &dyn Fn() -> bool) -> R<()> {
        if self.engine == "lightpanda" {
            return err(
                "BrowserError",
                "This browser engine (Lightpanda) does not render screenshots; use browser_observe",
            );
        }
        let r = self.call(
            "Page.captureScreenshot",
            json!({"format": "png"}),
            cancelled,
        )?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(r["data"].as_str().unwrap_or_default())
            .or_else(|e| err("BrowserError", e))?;
        fs::write(path, bytes).or_else(|e| super::io_err(e, path))
    }

    /// Dropping the browser kills Chrome's process group and deletes the profile.
    pub fn close(self) {}
}

/// Kills Chrome's process group and removes its throwaway profile.
struct KillGuard(Option<Child>, PathBuf);

impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            crate::runtime::terminate_group(&mut c);
        }
        let _ = fs::remove_dir_all(&self.1);
    }
}
