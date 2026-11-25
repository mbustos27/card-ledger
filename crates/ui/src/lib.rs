//! The card-ledger window on 127.0.0.1:8785.
//!
//! A small web server that only this computer can reach, serving one page and a JSON API over the
//! ledger database.
//!
//! Safety (the gift-check model): it binds to 127.0.0.1 only; it refuses requests whose Host isn't
//! this server (blocks DNS-rebinding pages); API reads refuse other origins; every POST must carry
//! the `X-Card-Ledger: 1` header and a JSON body, which a page on another site can't send without
//! a CORS preflight this server never approves; the page loads no outside scripts or fonts, and the
//! CSP only lets images come from `https://assets.tcgdex.net`. Catalogue text is only ever shown
//! with `textContent`. The JustTCG key never reaches the window: the Prices tile gets at most its
//! last four characters, and nothing here can set or change the key.

use card_ledger_core::db::Db;
use card_ledger_core::ledger::{self, CardView, LedgerView};
use card_ledger_core::model::Condition;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");

pub const DEFAULT_PORT: u16 = 8785;
/// The custom header every API call from the window carries.
pub const MARKER: &str = "X-Card-Ledger";
/// The only host card images may load from.
pub const IMAGE_HOST: &str = "https://assets.tcgdex.net";
/// Largest request body accepted (the API only takes tiny JSON objects).
const BODY_LIMIT: usize = 64 * 1024;
/// Request-handling threads: a price refresh holds one, the rest keep the window working.
const WORKERS: usize = 4;

/// What "Refresh prices" runs. It gets the data folder (the ledger database is
/// `<data_dir>/ledger.db`) so it can open its own connection and the window keeps answering while
/// it works. It returns the report text shown in the window, or an error message.
pub type RefreshHook = Arc<dyn Fn(&Path) -> Result<String, String> + Send + Sync>;

/// Whether per-condition prices (JustTCG) are on, as the window shows it. The window never gets
/// the key: at most its last four characters, and [`App`] cuts `ends` to four again before
/// sending it. There is no way to set the key from the window; `card-ledger justtcg-key set` is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionKey {
    pub on: bool,
    /// The key's last four characters.
    pub ends: Option<String>,
    /// Why it is off when a key couldn't be read (never contains the key).
    pub problem: Option<String>,
}

/// Reports the JustTCG key's status for the Prices tile.
pub type KeyStatusHook = Arc<dyn Fn() -> ConditionKey + Send + Sync>;

/// The at most four last characters of `ends`, and only when they are plain printable ASCII.
fn last_four(ends: &str) -> Option<String> {
    let chars: Vec<char> = ends.trim().chars().collect();
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    (!tail.is_empty() && tail.bytes().all(|b| b.is_ascii_graphic())).then_some(tail)
}

/// `card-ledger ui [--no-open] [--port N]`: serves until Ctrl+C. Price refresh isn't wired.
pub fn main(data_dir: &Path, args: &[String]) -> i32 {
    main_with(data_dir, args, None, None)
}

/// Same as [`main`], with the function "Refresh prices" calls and the JustTCG key status.
pub fn main_with(
    data_dir: &Path,
    args: &[String],
    refresh: Option<RefreshHook>,
    key_status: Option<KeyStatusHook>,
) -> i32 {
    let mut open = true;
    let mut no_images = false;
    let mut port = DEFAULT_PORT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-open" => open = false,
            "--no-images" => no_images = true,
            "--port" => match it
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|p| *p >= 1024)
            {
                Some(p) => port = p,
                None => {
                    eprintln!("card-ledger ui: --port needs a number from 1024 to 65535");
                    return 2;
                }
            },
            other => {
                eprintln!("card-ledger ui: unknown option {other:?} (use --no-open, --no-images, --port N)");
                return 2;
            }
        }
    }
    let app = match App::open(data_dir, port, refresh)
        .map(|a| a.with_key_status(key_status).with_no_images(no_images))
    {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("card-ledger ui: {e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/");
    let server = match tiny_http::Server::http(("127.0.0.1", port)) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("card-ledger ui: can't use port {port} ({e}). If card-ledger is already running, use {url}");
            return 2;
        }
    };
    println!("card-ledger window: {url}  (Ctrl+C here to stop)");
    if open {
        open_browser(&url);
    }
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (server, app) = (Arc::clone(&server), Arc::clone(&app));
            std::thread::spawn(move || {
                for req in server.incoming_requests() {
                    // One bad request mustn't take the whole server down.
                    let _ =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.handle(req)));
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    0
}

pub fn open_browser(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if r.is_err() {
        println!("Open {url} in your browser.");
    }
}

pub struct App {
    pub port: u16,
    data_dir: PathBuf,
    db: Mutex<Db>,
    refresh: Option<RefreshHook>,
    key_status: Option<KeyStatusHook>,
    /// Held while a refresh runs, so only one runs at a time.
    refreshing: Mutex<()>,
    /// `--no-images`: the view carries no image URLs.
    no_images: bool,
}

/// A reply before it becomes an HTTP response, so tests can look at it.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

fn ok_json(v: Value) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json; charset=utf-8",
        body: v.to_string(),
    }
}

fn err_json(status: u16, msg: impl Into<String>) -> Reply {
    Reply {
        status,
        content_type: "application/json; charset=utf-8",
        body: json!({ "error": msg.into() }).to_string(),
    }
}

/// The ledger the window shows. The one place to swap in another builder with the same shape.
fn view(app: &App) -> Result<LedgerView, String> {
    let db = app.db();
    let mut v = ledger::build_view(&db).map_err(|e| format!("can't read the ledger: {e}"))?;
    if app.no_images {
        for c in &mut v.cards {
            c.image = None;
        }
    }
    Ok(v)
}

impl App {
    /// Opens `<data_dir>/ledger.db` (created if missing).
    pub fn open(data_dir: &Path, port: u16, refresh: Option<RefreshHook>) -> Result<App, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("can't create {}: {e}", data_dir.display()))?;
        let path = data_dir.join("ledger.db");
        let db = Db::open(&path).map_err(|e| format!("can't open {}: {e}", path.display()))?;
        Ok(App::with(db, data_dir, port, refresh))
    }

    pub fn with(db: Db, data_dir: &Path, port: u16, refresh: Option<RefreshHook>) -> App {
        App {
            port,
            data_dir: data_dir.to_path_buf(),
            db: Mutex::new(db),
            refresh,
            key_status: None,
            refreshing: Mutex::new(()),
            no_images: false,
        }
    }

    /// Placeholders instead of card art (for screenshots, or to load nothing from TCGdex's
    /// image host).
    pub fn with_no_images(mut self, no_images: bool) -> App {
        self.no_images = no_images;
        self
    }

    /// Sets what the Prices tile asks for the JustTCG key status (without it: "off").
    pub fn with_key_status(mut self, hook: Option<KeyStatusHook>) -> App {
        self.key_status = hook;
        self
    }

    /// `{"on": bool, "ends": "abcd" | null, "problem": "…" | null}` for the window.
    fn condition_key_json(&self) -> Value {
        let k = self.key_status.as_ref().map(|h| h()).unwrap_or_default();
        let ends = if k.on {
            k.ends.as_deref().and_then(last_four)
        } else {
            None
        };
        json!({ "on": k.on, "ends": ends, "problem": if k.on { None } else { k.problem } })
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        // A panic elsewhere mustn't lock the window out of its own database.
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The headers every response carries. Images may only come from the TCGdex asset host.
    pub fn security_headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("X-Content-Type-Options", "nosniff".into()),
            ("Cache-Control", "no-store".into()),
            ("Referrer-Policy", "no-referrer".into()),
            ("X-Frame-Options", "DENY".into()),
            (
                "Content-Security-Policy",
                format!(
                    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' {IMAGE_HOST}; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
                ),
            ),
        ]
    }

    fn handle(&self, mut req: tiny_http::Request) {
        let method = req.method().as_str().to_string();
        let url = req.url().to_string();
        let header = |name: &str| {
            req.headers()
                .iter()
                .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
                .map(|h| h.value.as_str().to_string())
        };
        let (host, marker, ctype, origin) = (
            header("Host"),
            header(MARKER),
            header("Content-Type"),
            header("Origin"),
        );
        let mut body = String::new();
        let mut too_big = false;
        if method == "POST" {
            let mut buf = Vec::new();
            let _ = req
                .as_reader()
                .take(BODY_LIMIT as u64 + 1)
                .read_to_end(&mut buf);
            too_big = buf.len() > BODY_LIMIT;
            body = String::from_utf8_lossy(&buf).into_owned();
        }
        let reply = if too_big {
            err_json(413, "request too large")
        } else {
            self.route(
                &method,
                &url,
                host.as_deref(),
                marker.as_deref(),
                ctype.as_deref(),
                origin.as_deref(),
                &body,
            )
        };
        let mut resp = tiny_http::Response::from_string(reply.body).with_status_code(reply.status);
        let mut headers = vec![("Content-Type", reply.content_type.to_string())];
        headers.extend(self.security_headers());
        for (k, v) in headers {
            if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                resp.add_header(h);
            }
        }
        let _ = req.respond(resp);
    }

    /// Everything except the socket, so it can be tested directly.
    #[allow(clippy::too_many_arguments)]
    pub fn route(
        &self,
        method: &str,
        url: &str,
        host: Option<&str>,
        marker: Option<&str>,
        ctype: Option<&str>,
        origin: Option<&str>,
        body: &str,
    ) -> Reply {
        let own = [
            format!("127.0.0.1:{}", self.port),
            format!("localhost:{}", self.port),
        ];
        if !host.is_some_and(|h| own.iter().any(|o| o.eq_ignore_ascii_case(h))) {
            return err_json(421, "wrong host");
        }
        let ours = |o: &str| {
            own.iter()
                .any(|h| o.eq_ignore_ascii_case(&format!("http://{h}")))
        };
        let path = url.split('?').next().unwrap_or("");
        if path.starts_with("/api/") {
            if origin.is_some_and(|o| !ours(o)) {
                return err_json(403, "requests must come from the card-ledger window");
            }
            if method == "POST"
                && (marker != Some("1")
                    || !ctype.is_some_and(|c| c.starts_with("application/json")))
            {
                return err_json(403, "requests must come from the card-ledger window");
            }
        }
        let text = |ct: &'static str, b: &str| Reply {
            status: 200,
            content_type: ct,
            body: b.into(),
        };
        match (method, path) {
            ("GET", "/") => return text("text/html; charset=utf-8", INDEX),
            ("GET", "/app.js") => return text("text/javascript; charset=utf-8", APP_JS),
            ("GET", "/style.css") => return text("text/css; charset=utf-8", STYLE),
            ("GET", "/api/ledger") => {
                return match view(self)
                    .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))
                {
                    Ok(mut v) => {
                        if let Some(o) = v.as_object_mut() {
                            o.insert("justtcg".into(), self.condition_key_json());
                        }
                        ok_json(v)
                    }
                    Err(e) => err_json(500, e),
                }
            }
            ("POST", "/api/refresh") => return self.refresh(),
            ("POST", p) if p.starts_with("/api/copy/") => return self.copy_post(p, body),
            ("GET", _) | ("POST", _) => {}
            _ => return err_json(405, "method not allowed"),
        }
        err_json(404, "not found")
    }

    /// `POST /api/copy/{id}/condition {"condition":"LP"}` and `POST /api/copy/{id}/pulled {"pulled":true}`.
    fn copy_post(&self, path: &str, body: &str) -> Reply {
        let rest = &path["/api/copy/".len()..];
        let Some((id, what)) = rest.split_once('/') else {
            return err_json(404, "not found");
        };
        let Some(id) = id
            .parse::<i64>()
            .ok()
            .filter(|_| id.bytes().all(|b| b.is_ascii_digit()))
        else {
            return err_json(404, "not found");
        };
        if what != "condition" && what != "pulled" {
            return err_json(404, "not found");
        }
        let v: Value = match serde_json::from_str(body) {
            Ok(v @ Value::Object(_)) => v,
            _ => return err_json(400, "the body must be a JSON object"),
        };
        let changed = {
            let db = self.db();
            if what == "condition" {
                let cond = v.get("condition").and_then(Value::as_str).and_then(|s| {
                    Condition::ALL
                        .into_iter()
                        .find(|c| c.code().eq_ignore_ascii_case(s.trim()))
                });
                let Some(cond) = cond else {
                    return err_json(
                        400,
                        "condition must be one of unchecked, NM, LP, MP, HP, DMG",
                    );
                };
                db.set_condition(id, cond)
            } else {
                let Some(p) = v.get("pulled").and_then(Value::as_bool) else {
                    return err_json(400, "pulled must be true or false");
                };
                db.set_pulled(id, p)
            }
        };
        match changed {
            Ok(true) => {}
            Ok(false) => return err_json(404, format!("no card with copy id {id}")),
            Err(e) => return err_json(500, format!("can't save: {e}")),
        }
        match view(self) {
            Ok(v) => match v.cards.into_iter().find(|c: &CardView| c.copy_id == id) {
                Some(card) => ok_json(json!(card)),
                None => err_json(
                    500,
                    format!("saved, but copy {id} is missing from the ledger view"),
                ),
            },
            Err(e) => err_json(500, e),
        }
    }

    fn refresh(&self) -> Reply {
        let Some(hook) = &self.refresh else {
            return err_json(501, "price refresh isn't wired yet");
        };
        let Ok(_guard) = self.refreshing.try_lock() else {
            return err_json(409, "a price refresh is already running");
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(&self.data_dir))) {
            Ok(Ok(report)) => ok_json(json!({ "report": report })),
            Ok(Err(e)) => err_json(502, e),
            Err(_) => err_json(500, "the price refresh stopped with an internal error"),
        }
    }
}

#[cfg(test)]
mod tests;
