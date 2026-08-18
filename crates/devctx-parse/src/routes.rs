//! Framework-aware HTTP route extraction (regex-based).
//!
//! Best-effort extractors for FastAPI, Flask, Express, NestJS, Spring, Quarkus
//! (JAX-RS) and Angular router configs. See `docs/architecture-spec.md` §3.
//! The framework is auto-detected from extension + content;
//! class-level prefixes are applied where a single one is evident.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

/// An extracted route (a handler mapped to an HTTP method + path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Framework the route was found in.
    pub framework: String,
    /// HTTP method (uppercased), or empty (e.g. Angular client routes).
    pub http_method: String,
    /// Route path (with class-level prefix applied when known).
    pub path: String,
    /// Handler class, if known.
    pub handler_class: String,
    /// Handler method/function, if known.
    pub handler_method: String,
    /// `Class.method` or `method`, for reverse lookup.
    pub handler_symbol: String,
    /// Source file.
    pub file: String,
    /// 1-based line.
    pub line: u32,
}

/// Extract routes from `source`, auto-detecting the framework by `path` + content.
pub fn extract_routes(source: &str, path: &Path) -> Vec<Route> {
    let file = path.to_string_lossy().to_string();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "py" => {
            if source.contains("APIRouter")
                || source.contains("FastAPI")
                || RE_FASTAPI.is_match(source)
            {
                fastapi(source, &file)
            } else if source.contains(".route(") || source.contains("Flask") {
                flask(source, &file)
            } else {
                Vec::new()
            }
        }
        "js" | "mjs" | "cjs" | "jsx" => express(source, &file),
        "ts" | "tsx" => {
            if source.contains("@Controller") {
                nest(source, &file)
            } else if source.contains("component:") || source.contains("loadChildren") {
                angular(source, &file)
            } else {
                express(source, &file)
            }
        }
        "java" => {
            if source.contains("javax.ws.rs")
                || source.contains("jakarta.ws.rs")
                || RE_JAXRS.is_match(source)
            {
                quarkus(source, &file)
            } else {
                spring(source, &file)
            }
        }
        "kt" => spring(source, &file),
        _ => Vec::new(),
    }
}

fn line_of(source: &str, byte: usize) -> u32 {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Find the first `def <name>` / method name after `from` (Python handler).
fn py_handler_after(source: &str, from: usize) -> String {
    RE_PY_DEF
        .captures(&source[from.min(source.len())..])
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

fn symbol(class: &str, method: &str) -> String {
    match (class.is_empty(), method.is_empty()) {
        (false, false) => format!("{class}.{method}"),
        (true, false) => method.to_string(),
        _ => String::new(),
    }
}

static RE_WORD_PAREN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\w+)\s*\(").unwrap());
static RE_CLASS_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:class|interface|enum)\s+(\w+)").unwrap());
static RE_EXPRESS_HANDLER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*,\s*([A-Za-z_$][\w.$]*)\s*[),]").unwrap());

/// Words that look like `name(` but are not method declarations.
const CTRL_KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "new",
    "synchronized",
    "function",
];

/// The next method-declaration name after `from` (Java/Kotlin/TS): the first
/// `name(` not preceded by `@`/`.` and not a control keyword.
fn method_after(source: &str, from: usize) -> String {
    let sub = &source[from.min(source.len())..];
    for c in RE_WORD_PAREN.captures_iter(sub) {
        let g = c.get(1).unwrap();
        let word = g.as_str();
        if CTRL_KEYWORDS.contains(&word) {
            continue;
        }
        let prev = sub[..g.start()].trim_end().chars().last();
        if matches!(prev, Some('@') | Some('.')) {
            continue;
        }
        return word.to_string();
    }
    String::new()
}

/// The nearest class/interface/enum declared before `pos`.
fn enclosing_class_before(source: &str, pos: usize) -> String {
    RE_CLASS_DECL
        .captures_iter(&source[..pos.min(source.len())])
        .last()
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

/// A named Express handler reference (`handler` or `controller.list`) following
/// the route path; empty for inline/arrow functions.
fn express_handler(source: &str, after: usize) -> String {
    RE_EXPRESS_HANDLER
        .captures(&source[after.min(source.len())..])
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

/// Split a `Class.method` (or `object.method`) handler into (class, method).
fn split_handler(handler: &str) -> (String, String) {
    match handler.rsplit_once('.') {
        Some((c, m)) => (c.to_string(), m.to_string()),
        None => (String::new(), handler.to_string()),
    }
}

/// Build the `(class, method, symbol)` triple for an annotation at `end`/`start`.
fn annotated_handler(source: &str, ann_start: usize, ann_end: usize) -> (String, String, String) {
    let method = method_after(source, ann_end);
    let class = enclosing_class_before(source, ann_start);
    let sym = symbol(&class, &method);
    (class, method, sym)
}

// --- FastAPI ---
static RE_FASTAPI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"@(?:app|router)\.(get|post|put|delete|patch|options|head)\s*\(\s*["']([^"']*)["']"#,
    )
    .unwrap()
});

fn fastapi(source: &str, file: &str) -> Vec<Route> {
    RE_FASTAPI
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            let method = c[1].to_uppercase();
            let path = c[2].to_string();
            let handler = py_handler_after(source, m.end());
            Route {
                framework: "fastapi".into(),
                http_method: method,
                path,
                handler_class: String::new(),
                handler_method: handler.clone(),
                handler_symbol: symbol("", &handler),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Flask ---
static RE_FLASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@\w+\.route\s*\(\s*["']([^"']*)["']([^)]*)\)"#).unwrap());
static RE_METHODS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"methods\s*=\s*\[([^\]]*)\]"#).unwrap());
static RE_QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"["']([^"']+)["']"#).unwrap());

fn flask(source: &str, file: &str) -> Vec<Route> {
    let mut out = Vec::new();
    for c in RE_FLASK.captures_iter(source) {
        let m = c.get(0).unwrap();
        let path = c[1].to_string();
        let methods: Vec<String> = RE_METHODS
            .captures(&c[2])
            .map(|mc| {
                RE_QUOTED
                    .captures_iter(&mc[1])
                    .map(|q| q[1].to_uppercase())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| vec!["GET".to_string()]);
        let handler = py_handler_after(source, m.end());
        let line = line_of(source, m.start());
        for method in methods {
            out.push(Route {
                framework: "flask".into(),
                http_method: method,
                path: path.clone(),
                handler_class: String::new(),
                handler_method: handler.clone(),
                handler_symbol: symbol("", &handler),
                file: file.to_string(),
                line,
            });
        }
    }
    out
}

// --- Express ---
static RE_EXPRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(?:app|router)\.(get|post|put|delete|patch|all)\s*\(\s*["'`]([^"'`]*)["'`]"#)
        .unwrap()
});

fn express(source: &str, file: &str) -> Vec<Route> {
    RE_EXPRESS
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            let handler = express_handler(source, m.end());
            let (hc, hm) = split_handler(&handler);
            Route {
                framework: "express".into(),
                http_method: c[1].to_uppercase(),
                path: c[2].to_string(),
                handler_class: hc,
                handler_method: hm,
                handler_symbol: handler,
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- NestJS ---
static RE_CONTROLLER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@Controller\s*\(\s*["']([^"']*)["']"#).unwrap());
static RE_NEST_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@(Get|Post|Put|Delete|Patch)\s*\(\s*(?:["']([^"']*)["'])?\s*\)"#).unwrap()
});

fn nest(source: &str, file: &str) -> Vec<Route> {
    let prefix = RE_CONTROLLER
        .captures(source)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    RE_NEST_METHOD
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            let sub = c.get(2).map(|x| x.as_str()).unwrap_or("");
            let (hc, hm, hs) = annotated_handler(source, m.start(), m.end());
            Route {
                framework: "nestjs".into(),
                http_method: c[1].to_uppercase(),
                path: join_path(&prefix, sub),
                handler_class: hc,
                handler_method: hm,
                handler_symbol: hs,
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Spring ---
static RE_SPRING_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@RequestMapping\s*\(\s*(?:value\s*=\s*|path\s*=\s*)?["']([^"']*)["']"#).unwrap()
});
static RE_SPRING_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"@(Get|Post|Put|Delete|Patch)Mapping\s*\(\s*(?:value\s*=\s*|path\s*=\s*)?["']([^"']*)["']"#,
    )
    .unwrap()
});

fn spring(source: &str, file: &str) -> Vec<Route> {
    // Class-level prefix: first @RequestMapping not immediately followed by a method annotation.
    let prefix = RE_SPRING_CLASS
        .captures(source)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    RE_SPRING_METHOD
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            let (hc, hm, hs) = annotated_handler(source, m.start(), m.end());
            Route {
                framework: "spring".into(),
                http_method: c[1].to_uppercase(),
                path: join_path(&prefix, &c[2]),
                handler_class: hc,
                handler_method: hm,
                handler_symbol: hs,
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Quarkus (JAX-RS) ---
static RE_JAXRS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)\b"#).unwrap());
static RE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@Path\s*\(\s*["']([^"']*)["']"#).unwrap());

fn quarkus(source: &str, file: &str) -> Vec<Route> {
    let paths: Vec<_> = RE_PATH.captures_iter(source).collect();
    // The first @Path is treated as the class-level prefix.
    let class_prefix = paths.first().map(|c| c[1].to_string()).unwrap_or_default();
    RE_JAXRS
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            // A method-level @Path following the HTTP annotation (within ~200 bytes).
            let window = window_after(source, m.end(), 200);
            let sub = RE_PATH
                .captures(window)
                .map(|p| p[1].to_string())
                .unwrap_or_default();
            let (hc, hm, hs) = annotated_handler(source, m.start(), m.end());
            Route {
                framework: "quarkus".into(),
                http_method: c[1].to_uppercase(),
                path: join_path(&class_prefix, &sub),
                handler_class: hc,
                handler_method: hm,
                handler_symbol: hs,
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

// --- Angular router config ---
static RE_NG_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"path\s*:\s*["']([^"']*)["']"#).unwrap());

fn angular(source: &str, file: &str) -> Vec<Route> {
    RE_NG_PATH
        .captures_iter(source)
        .map(|c| {
            let m = c.get(0).unwrap();
            Route {
                framework: "angular".into(),
                http_method: String::new(),
                path: c[1].to_string(),
                handler_class: String::new(),
                handler_method: String::new(),
                handler_symbol: String::new(),
                file: file.to_string(),
                line: line_of(source, m.start()),
            }
        })
        .collect()
}

static RE_PY_DEF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:async\s+)?def\s+(\w+)\s*\("#).unwrap());

/// Join a class-level prefix and a method sub-path into a single leading-slash path.
fn join_path(prefix: &str, sub: &str) -> String {
    let mut parts = Vec::new();
    let p = prefix.trim_matches('/');
    let s = sub.trim_matches('/');
    if !p.is_empty() {
        parts.push(p);
    }
    if !s.is_empty() {
        parts.push(s);
    }
    format!("/{}", parts.join("/"))
}

/// The `len` bytes of `source` following `start`, clamped to a char boundary.
///
/// Slicing at `start + len` directly panics whenever that offset lands inside a
/// multi-byte character — which any accented word ("configuración") makes
/// routine in a non-English codebase. Walking the end back to the nearest
/// boundary keeps the window slightly shorter instead of aborting the index.
fn window_after(source: &str, start: usize, len: usize) -> &str {
    if start >= source.len() {
        return "";
    }
    let mut end = (start + len).min(source.len());
    while end > start && !source.is_char_boundary(end) {
        end -= 1;
    }
    &source[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes(src: &str, name: &str) -> Vec<Route> {
        extract_routes(src, Path::new(name))
    }

    #[test]
    fn fastapi_routes() {
        let src = "\
from fastapi import APIRouter
router = APIRouter()

@router.get(\"/users\")
async def list_users():
    return []

@router.post(\"/users\")
def create_user():
    return {}
";
        let r = routes(src, "api.py");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "fastapi");
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].path, "/users");
        assert_eq!(r[0].handler_method, "list_users");
        assert_eq!(r[1].http_method, "POST");
    }

    #[test]
    fn flask_routes_with_methods() {
        let src = "\
from flask import Flask
app = Flask(__name__)

@app.route(\"/login\", methods=[\"GET\", \"POST\"])
def login():
    return \"ok\"
";
        let r = routes(src, "app.py");
        assert_eq!(r.len(), 2);
        assert!(r
            .iter()
            .all(|x| x.path == "/login" && x.framework == "flask"));
        assert!(r.iter().any(|x| x.http_method == "GET"));
        assert!(r.iter().any(|x| x.http_method == "POST"));
        assert_eq!(r[0].handler_method, "login");
    }

    #[test]
    fn express_routes() {
        let src = "const router = require('express').Router();\nrouter.get('/health', (req, res) => res.send('ok'));\napp.post('/items', handler);\n";
        let r = routes(src, "routes.js");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].path, "/health");
        assert_eq!(r[0].handler_symbol, ""); // inline arrow function
        assert_eq!(r[1].http_method, "POST");
        assert_eq!(r[1].handler_symbol, "handler"); // named handler
    }

    #[test]
    fn spring_routes_with_prefix() {
        let src = "\
@RestController
@RequestMapping(\"/api\")
public class UserController {
    @GetMapping(\"/users\")
    public List<User> list() { return null; }
    @PostMapping(\"/users\")
    public User create() { return null; }
}
";
        let r = routes(src, "UserController.java");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "spring");
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].handler_method, "list");
        assert_eq!(r[0].handler_symbol, "UserController.list");
        assert_eq!(r[1].handler_symbol, "UserController.create");
    }

    #[test]
    fn spring_kotlin_routes() {
        let src = "\
@RestController
@RequestMapping(\"/api\")
class UserController {
    @GetMapping(\"/users\")
    fun list(): List<User> { return emptyList() }
}
";
        let r = routes(src, "UserController.kt");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].framework, "spring");
        assert_eq!(r[0].path, "/api/users");
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].handler_symbol, "UserController.list");
    }

    #[test]
    fn quarkus_jaxrs_routes() {
        let src = "\
import jakarta.ws.rs.GET;
@Path(\"/greet\")
public class GreetResource {
    @GET
    @Path(\"/hello\")
    public String hello() { return \"hi\"; }
}
";
        let r = routes(src, "GreetResource.java");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].framework, "quarkus");
        assert_eq!(r[0].http_method, "GET");
        assert_eq!(r[0].path, "/greet/hello");
        assert_eq!(r[0].handler_symbol, "GreetResource.hello");
    }

    #[test]
    fn nest_routes_with_controller_prefix() {
        let src = "\
@Controller('cats')
export class CatsController {
    @Get()
    findAll() {}
    @Post('/create')
    create() {}
}
";
        let r = routes(src, "cats.controller.ts");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "nestjs");
        assert_eq!(r[0].path, "/cats");
        assert_eq!(r[0].handler_symbol, "CatsController.findAll");
        assert_eq!(r[1].path, "/cats/create");
        assert_eq!(r[1].handler_method, "create");
    }

    #[test]
    fn window_after_stops_at_a_char_boundary() {
        // 'ó' occupies two bytes, so a window ending at byte 2 would split it.
        let s = "aó";
        assert_eq!(window_after(s, 0, 2), "a");
        assert_eq!(window_after(s, 0, 3), "aó");
        assert_eq!(window_after(s, 0, 99), "aó");
        assert_eq!(window_after(s, 99, 10), "");
    }

    #[test]
    fn jaxrs_routes_survive_accents_in_the_lookahead_window() {
        // The @Path lookahead reads ~200 bytes past the annotation; padding it
        // with accented text puts a multi-byte character on that boundary,
        // which used to panic and abort the whole index.
        let padding = "ó".repeat(150);
        let src = format!(
            "@Path(\"/orders\")\npublic class R {{\n  // {padding}\n  @GET\n  @Path(\"/search\")\n  public String search() {{ return null; }}\n}}\n"
        );
        let r = routes(&src, "R.java");
        assert!(r.iter().any(|x| x.path.contains("orders")));
    }

    #[test]
    fn angular_routes() {
        let src = "const routes: Routes = [\n  { path: 'home', component: HomeComponent },\n  { path: 'about', loadChildren: () => x },\n];\n";
        let r = routes(src, "app-routing.module.ts");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].framework, "angular");
        assert_eq!(r[0].path, "home");
    }
}
